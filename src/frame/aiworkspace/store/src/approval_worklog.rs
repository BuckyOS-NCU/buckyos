use crate::approval_capture::{approved_content_folder, is_container, live_shared, shared_descendant};
use crate::docdb::{db_err, SqlCtx};
use crate::workspace::{Caller, CommitOpts, Workspace};
use aiworkspace_core::access::Cap;
use aiworkspace_core::model::{EntityRow, ReadCtx, SURFACES_ID, TYPE_ANNOTATION, TYPE_ASSET, TYPE_CELL};
use aiworkspace_core::plan::PlannedOp;
use aiworkspace_core::{Code, WsError, WsResult};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;

pub(crate) const DDL: &str = "
CREATE TABLE IF NOT EXISTS approval_work_events (
    event_seq INTEGER PRIMARY KEY AUTOINCREMENT,
    epoch TEXT NOT NULL,
    surface_id TEXT NOT NULL,
    at_ms INTEGER NOT NULL,
    principal TEXT NOT NULL,
    role TEXT NOT NULL CHECK(role IN ('student', 'teacher')),
    kind TEXT NOT NULL CHECK(kind IN ('frame_created', 'asset_uploaded', 'asset_downloaded', 'teacher_replied')),
    target_label TEXT,
    summary TEXT,
    commit_seq INTEGER
);
CREATE INDEX IF NOT EXISTS approval_work_events_surface
    ON approval_work_events(epoch, surface_id, event_seq);
";

pub const MAX_PAGE_SIZE: usize = 50;
const MAX_PATH: usize = 64;

#[derive(Debug, Clone, Serialize)]
pub struct WorkLogCutoff {
    pub epoch: String,
    pub last_event_seq: u64,
}

#[derive(Debug)]
pub struct WorkLogEvent {
    pub event_seq: u64,
    pub at_ms: i64,
    pub principal: String,
    pub role: String,
    pub kind: String,
    pub target_label: Option<String>,
    pub summary: Option<String>,
}

pub struct WorkLogPage {
    pub events: Vec<WorkLogEvent>,
    pub next_before: Option<u64>,
}

pub struct WorkLogQuery<'a> {
    pub cutoff_ms: Option<i64>,
    pub cutoff: Option<(&'a str, u64)>,
    pub before: Option<u64>,
    pub limit: usize,
    pub principals: &'a [String],
}

fn scrub_label(value: Option<&str>, max: usize) -> Option<String> {
    let text = value?.trim();
    if text.is_empty() {
        return None;
    }
    let mut out = String::new();
    for c in text
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r'))
    {
        if out.len() + c.len_utf8() > max {
            break;
        }
        out.push(c);
    }
    (!out.is_empty()).then_some(out)
}

fn label(entity: &EntityRow) -> Option<String> {
    scrub_label(
        entity
            .payload
            .get("title")
            .and_then(Value::as_str)
            .or_else(|| entity.payload.get("file_name").and_then(Value::as_str))
            .or(entity.name.as_deref()),
        360,
    )
}

fn scoped_surface(ctx: &SqlCtx<'_>, entity_id: &str) -> WsResult<Option<String>> {
    let mut current = entity_id.to_owned();
    for _ in 0..MAX_PATH {
        let Some(row) = ctx.entity(&current)?.filter(live_shared) else {
            return Ok(None);
        };
        if is_container(&row, "surface") {
            return Ok(shared_descendant(ctx, &current, SURFACES_ID)?.then_some(current));
        }
        if is_container(&row, "folder") && row.payload.get("system").and_then(Value::as_str) == Some("surface_content")
        {
            let Some(surface_id) = row.payload.get("surface_id").and_then(Value::as_str) else {
                return Ok(None);
            };
            let Some(s) = ctx
                .entity(surface_id)?
                .filter(live_shared)
                .filter(|s| is_container(s, "surface"))
            else {
                return Ok(None);
            };
            let bound = approved_content_folder(ctx, &s)?.as_deref() == Some(current.as_str())
                && shared_descendant(ctx, surface_id, SURFACES_ID)?;
            return Ok(bound.then(|| surface_id.to_owned()));
        }
        let Some(edge) = ctx.edge(&current)? else {
            return Ok(None);
        };
        current = edge.parent_id;
    }
    Ok(None)
}

impl Workspace {
    pub(crate) fn has_recorded_student_seat(&self, caller: &Caller) -> WsResult<bool> {
        if !caller.is_direct_workspace_account() || caller.has_live_ncu_pbl_teacher() {
            return Ok(false);
        }
        self.local.query_row(
            "SELECT 1 FROM classroom_entries WHERE kind='student' AND recipient=?1 AND consumed_at IS NOT NULL LIMIT 1",
            [caller.principal.as_str()], |_| Ok(()),
        ).optional().map(|row| row.is_some()).map_err(db_err)
    }

    fn has_student_seat(&self, caller: &Caller, surface: &str) -> WsResult<bool> {
        if !caller.is_direct_workspace_account() || caller.has_live_ncu_pbl_teacher() {
            return Ok(false);
        }
        // A redeemed invitation is closed. Its exact membership row remains until revoked.
        self.local
            .query_row(
                "SELECT 1 FROM classroom_entries c
             JOIN share_entries s ON s.entry_id=c.entry_id
             JOIN acl_members m ON m.group_id=s.group_id AND m.via_entry=c.entry_id AND m.member=c.recipient
             WHERE c.kind='student' AND c.consumed_at IS NOT NULL AND c.recipient=?1 AND c.scope_id=?2
             LIMIT 1",
                params![caller.principal, surface],
                |_| Ok(()),
            )
            .optional()
            .map(|row| row.is_some())
            .map_err(db_err)
    }

    fn record_approval_event(
        &self,
        doc: &Connection,
        at_ms: i64,
        caller: &Caller,
        surface: &str,
        role: &str,
        kind: &str,
        target: Option<String>,
        summary: Option<String>,
        commit_seq: Option<u64>,
    ) -> WsResult<()> {
        doc.execute(
            "INSERT INTO approval_work_events(epoch,surface_id,at_ms,principal,role,kind,target_label,summary,commit_seq)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![self.epoch, surface, at_ms, caller.principal, role, kind, target, summary, commit_seq],
        ).map_err(db_err)?;
        Ok(())
    }

    /// This runs inside the document writer transaction after the accepted rows were installed.
    pub(crate) fn record_approval_commit(
        &self,
        tx: &Connection,
        caller: &Caller,
        opts: &CommitOpts,
        seq: u64,
        now: &str,
        ops: &[PlannedOp],
    ) -> WsResult<()> {
        if !caller.is_direct_workspace_account()
            || opts.internal
            || opts.import
            || opts.as_principal.is_some()
            || opts.as_source
            || opts.as_build
            || opts.as_session
            || opts.authorize_as.is_some()
            || self.purpose().is_some()
        {
            return Ok(());
        }
        let teacher = caller.has_live_ncu_pbl_teacher() && self.owner()?.as_deref() == Some(caller.principal.as_str());
        let at = aiworkspace_core::value::parse_datetime_ms(now)
            .ok_or_else(|| WsError::io("invalid accepted commit time"))?;
        let ctx = SqlCtx {
            conn: tx,
            local: Some(&self.local),
            cache: &self.cache,
            now,
        };
        let mut uploaded = BTreeSet::new();
        for item in ops.iter().filter(|item| item.op["op"] == "entity.create") {
            let Some(id) = item.op["entity_id"].as_str() else {
                continue;
            };
            let Some(entity) = ctx.entity(id)?.filter(live_shared) else {
                continue;
            };
            if teacher {
                if entity.type_id != TYPE_ANNOTATION
                    || entity.payload.get("kind").and_then(Value::as_str) != Some("note")
                {
                    continue;
                }
                let Some(target) = entity.payload.get("target").and_then(|v| v["entity_id"].as_str()) else {
                    continue;
                };
                let Some(parent) = ctx.entity(target)?.filter(live_shared).filter(|e| {
                    e.type_id == TYPE_ANNOTATION && e.payload.get("kind").and_then(Value::as_str) == Some("note")
                }) else {
                    continue;
                };
                let Some(surface) = scoped_surface(&ctx, target)? else {
                    continue;
                };
                if scoped_surface(&ctx, id)?.as_deref() != Some(surface.as_str()) {
                    continue;
                }
                let Some(body) = scrub_label(entity.payload.get("body").and_then(Value::as_str), 1200) else {
                    continue;
                };
                self.record_approval_event(
                    tx,
                    at,
                    caller,
                    &surface,
                    "teacher",
                    "teacher_replied",
                    label(&parent),
                    Some(body),
                    Some(seq),
                )?;
                continue;
            }
            let frame = (entity.type_id == TYPE_CELL
                && entity.payload.get("view").and_then(|v| v["type"].as_str()) == Some("frame"))
                || is_container(&entity, "pdf-frame");
            if !frame && entity.type_id != TYPE_ASSET {
                continue;
            }
            let Some(surface) = scoped_surface(&ctx, id)? else {
                continue;
            };
            if !self.has_student_seat(caller, &surface)? {
                continue;
            }
            let kind = if frame {
                "frame_created"
            } else {
                let Some(object) = entity.payload.get("object_id").and_then(Value::as_str) else {
                    continue;
                };
                let staged: Option<String> = self
                    .local
                    .query_row(
                        "SELECT principal FROM staged_assets WHERE object_id=?1 AND expires_at>?2",
                        params![object, now],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(db_err)?;
                if staged.as_deref() != Some(caller.principal.as_str())
                    || ctx.asset(object)?.is_none()
                    || !uploaded.insert((surface.clone(), object.to_owned()))
                {
                    continue;
                }
                "asset_uploaded"
            };
            self.record_approval_event(
                tx,
                at,
                caller,
                &surface,
                "student",
                kind,
                label(&entity),
                None,
                Some(seq),
            )?;
        }
        Ok(())
    }

    /// A separate explicit action, not a thumbnail, media stream or ordinary preview.
    pub fn download_approval_asset(&self, caller: &Caller, asset_entity_id: &str) -> WsResult<Vec<u8>> {
        let not_found = || WsError::not_found("classroom attachment not found");
        let now = self.now();
        let ctx = self.ctx(&now);
        let entity = ctx
            .entity(asset_entity_id)?
            .filter(|e| e.alive() && e.type_id == TYPE_ASSET)
            .ok_or_else(not_found)?;
        let surface = scoped_surface(&ctx, asset_entity_id)?;
        self.require_on(caller, Some(asset_entity_id), Cap::Read)?;
        let object_id = entity
            .payload
            .get("object_id")
            .and_then(Value::as_str)
            .ok_or_else(not_found)?;
        let info = ctx.asset(object_id)?.ok_or_else(not_found)?;
        // Read before recording success. No fetch or failed authorization produces an event.
        if self.objects.file_size(object_id)? != info.size {
            return Err(not_found());
        }
        let data = self.objects.file_head(object_id, info.size + 1)?;
        if data.len() as u64 != info.size {
            return Err(not_found());
        }
        if let Some(surface) = surface {
            if self.has_student_seat(caller, &surface)? {
                self.record_approval_event(
                    &self.doc,
                    (self.clock)(),
                    caller,
                    &surface,
                    "student",
                    "asset_downloaded",
                    label(&entity),
                    None,
                    None,
                )?;
            }
        }
        Ok(data)
    }

    pub fn approval_work_log_cutoff(&self, surface_id: &str) -> WsResult<WorkLogCutoff> {
        let last_event_seq = self
            .doc
            .query_row(
                "SELECT COALESCE(MAX(event_seq),0) FROM approval_work_events WHERE epoch=?1 AND surface_id=?2",
                params![self.epoch, surface_id],
                |row| row.get(0),
            )
            .map_err(db_err)?;
        Ok(WorkLogCutoff {
            epoch: self.epoch.clone(),
            last_event_seq,
        })
    }

    pub fn approval_work_log(&self, owner: &str, surface_id: &str, query: &WorkLogQuery<'_>) -> WsResult<WorkLogPage> {
        let now = self.now();
        self.approval_surface(&self.ctx(&now), owner, surface_id)?;
        if query.cutoff.is_some_and(|(epoch, _)| epoch != self.epoch) {
            return Err(WsError::new(Code::RevisionConflict, "archived work log epoch changed"));
        }
        let mut stmt = self
            .doc
            .prepare_cached(
                "SELECT event_seq,at_ms,principal,role,kind,target_label,summary FROM approval_work_events
             WHERE epoch=?1 AND surface_id=?2
               AND (?3 IS NULL OR at_ms<=?3) AND (?4 IS NULL OR event_seq<=?4)
               AND (?5 IS NULL OR event_seq<?5)
               AND principal IN (SELECT value FROM json_each(?6))
             ORDER BY event_seq DESC LIMIT ?7",
            )
            .map_err(db_err)?;
        let limit = query.limit.clamp(1, MAX_PAGE_SIZE);
        let principals = serde_json::to_string(query.principals).map_err(|_| WsError::io("invalid work log roster"))?;
        let mut events = stmt
            .query_map(
                params![
                    self.epoch,
                    surface_id,
                    query.cutoff_ms,
                    query.cutoff.map(|(_, seq)| seq),
                    query.before,
                    principals,
                    (limit + 1) as i64
                ],
                |row| {
                    Ok(WorkLogEvent {
                        event_seq: row.get(0)?,
                        at_ms: row.get(1)?,
                        principal: row.get(2)?,
                        role: row.get(3)?,
                        kind: row.get(4)?,
                        target_label: row.get(5)?,
                        summary: row.get(6)?,
                    })
                },
            )
            .map_err(db_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db_err)?;
        let more = events.len() > limit;
        events.truncate(limit);
        let next_before = more.then(|| events.last().expect("nonempty page").event_seq);
        Ok(WorkLogPage { events, next_before })
    }
}
