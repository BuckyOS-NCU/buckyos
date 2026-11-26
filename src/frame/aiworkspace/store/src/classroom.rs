//! Explicit classroom grants backed by the same ACL and share-entry transaction as ordinary shares.
use crate::acl::AuditItem;
use crate::docdb::db_err;
use crate::entries::EntryState;
use crate::workspace::{random_id, Caller, Workspace};
use aiworkspace_core::access::{Cap, CapSet};
use aiworkspace_core::model::{ReadCtx, CANVAS_CONTENT_ID, TYPE_CONTAINER};
use aiworkspace_core::{Code, Deny, WsError, WsResult};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

const INVITE_TTL_SECONDS: i64 = 24 * 60 * 60;
const MAX_OPEN: i64 = 256;

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClassroomKind {
    Guest,
    Student,
}

impl ClassroomKind {
    fn name(self) -> &'static str {
        match self {
            Self::Guest => "guest",
            Self::Student => "student",
        }
    }

    fn caps(self) -> CapSet {
        let read = CapSet::of(Cap::Read);
        match self {
            Self::Guest => read,
            Self::Student => read.union(CapSet::of(Cap::Structure)),
        }
    }
}

struct Target {
    roots: Vec<String>,
    surface_title: Option<String>,
}

struct Offer<'a> {
    entry: &'a str,
    group: &'a str,
    recipient: &'a str,
    scope: &'a str,
    kind: ClassroomKind,
    roots: &'a [String],
}

fn unavailable() -> WsError {
    WsError::refused(
        Code::NotFound,
        Deny::EntryUnavailable,
        "classroom access is unavailable",
    )
}

pub fn valid_recipient(principal: &str) -> bool {
    !principal.is_empty()
        && principal.len() <= 128
        && principal
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && !principal.eq_ignore_ascii_case("root")
        && !matches!(principal, "." | "..")
}

impl Workspace {
    pub fn require_classroom_owner(&self, caller: &Caller) -> WsResult<()> {
        if !caller.has_live_ncu_pbl_teacher()
            || !self.acl_data()?.ncu_pbl_teacher_owned
            || self.purpose().is_some()
        {
            return Err(unavailable());
        }
        self.require_owner(caller)?;
        Ok(())
    }

    fn require_classroom_recipient(&self, caller: &Caller) -> WsResult<()> {
        let acl = self.acl_data()?;
        if !caller.is_direct_workspace_account()
            || !valid_recipient(&caller.principal)
            || !acl.ncu_pbl_teacher_owned
            || self.purpose().is_some()
            || acl.owner.as_deref() == Some(caller.principal.as_str())
        {
            return Err(unavailable());
        }
        Ok(())
    }

    fn classroom_target(&self, scope: &str, kind: ClassroomKind) -> WsResult<Target> {
        let now = self.now();
        let ctx = self.ctx(&now);
        let root = ctx
            .entity(scope)?
            .filter(|e| e.alive() && e.type_id == TYPE_CONTAINER)
            .ok_or_else(unavailable)?;
        let title = root
            .payload
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("Classroom")
            .to_owned();
        let mut roots = vec![scope.to_owned()];
        let mut surface_title = None;
        match root.payload.get("kind").and_then(Value::as_str) {
            Some("surface") => {
                let folder_id = root
                    .payload
                    .get("content_folder_id")
                    .and_then(Value::as_str)
                    .ok_or_else(unavailable)?;
                let valid = ctx.entity(folder_id)?.is_some_and(|folder| {
                    folder.alive()
                        && folder.type_id == TYPE_CONTAINER
                        && folder.payload.get("kind").and_then(Value::as_str) == Some("folder")
                        && folder.payload.get("system").and_then(Value::as_str)
                            == Some("surface_content")
                        && folder.payload.get("surface_id").and_then(Value::as_str) == Some(scope)
                }) && ctx
                    .edge(folder_id)?
                    .is_some_and(|edge| edge.parent_id == CANVAS_CONTENT_ID);
                if !valid {
                    return Err(unavailable());
                }
                roots.push(folder_id.to_owned());
                surface_title = Some(title);
            }
            Some("folder")
                if kind == ClassroomKind::Student
                    && !aiworkspace_core::model::is_system_id(scope)
                    && root.payload.get("system").is_none() => {}
            _ => return Err(unavailable()),
        }
        Ok(Target {
            roots,
            surface_title,
        })
    }

    fn insert_offer(
        &self,
        tx: &Connection,
        teacher: &Caller,
        offer: &Offer<'_>,
    ) -> WsResult<()> {
        let now = self.now();
        let seconds = (self.clock)() / 1000;
        let open: i64 = tx.query_row(
            "SELECT count(*) FROM classroom_entries c JOIN share_entries s ON s.entry_id=c.entry_id
             WHERE c.consumed_at IS NULL AND s.state='active' AND s.expires_at>?1",
            [seconds], |row| row.get(0),
        ).map_err(db_err)?;
        if open >= MAX_OPEN {
            return Err(WsError::limit("too many active classroom invitations"));
        }
        tx.execute(
            "INSERT INTO acl_groups (group_id,title,description,created_by,created_at) VALUES (?1,?2,'',?3,?4)",
            params![offer.group, offer.kind.name(), teacher.principal, now],
        ).map_err(db_err)?;
        for scope in offer.roots {
            tx.execute(
                "INSERT INTO acl_entries (subject,scope_entity_id,allow,deny,updated_by,updated_at)
                 VALUES (?1,?2,?3,0,?4,?5)",
                params![
                    format!("@{}", offer.group),
                    scope,
                    offer.kind.caps().0,
                    teacher.principal,
                    now
                ],
            )
            .map_err(db_err)?;
        }
        tx.execute(
            "INSERT INTO share_entries (entry_id,group_id,methods,state,expires_at,max_joins,created_by,created_at)
             VALUES (?1,?2,0,'active',?3,1,?4,?5)",
            params![offer.entry, offer.group, seconds + INVITE_TTL_SECONDS, teacher.principal, seconds],
        ).map_err(db_err)?;
        tx.execute(
            "INSERT INTO classroom_entries (entry_id,recipient,kind,scope_id,backing_folder)
             VALUES (?1,?2,?3,?4,?5)",
            params![offer.entry, offer.recipient, offer.kind.name(), offer.scope, offer.roots.get(1)],
        ).map_err(db_err)?;
        Ok(())
    }

    pub fn classroom_issue(
        &self,
        teacher: &Caller,
        recipient: &str,
        scope: &str,
        kind: ClassroomKind,
    ) -> WsResult<Value> {
        self.require_classroom_owner(teacher)?;
        if !valid_recipient(recipient) || recipient == teacher.principal {
            return Err(unavailable());
        }
        let target = self.classroom_target(scope, kind)?;
        let local = random_id("se_");
        let group = random_id("classroom-");
        let offer = Offer {
            entry: &local,
            group: &group,
            recipient,
            scope,
            kind,
            roots: &target.roots,
        };
        self.acl_txn(teacher, |tx| {
            self.insert_offer(tx, teacher, &offer)?;
            Ok((
                (),
                vec![AuditItem {
                    action: "classroom.issue",
                    target: json!({"entity_id":scope,"kind":kind.name()}),
                    before: None,
                    after: None,
                }],
            ))
        })?;
        Ok(json!({"ok":true,"entry_id": self.entry_public_id(&local)}))
    }

    pub fn classroom_request(&self, caller: &Caller, surface_id: &str) -> WsResult<Value> {
        self.require_classroom_recipient(caller)?;
        self.classroom_target(surface_id, ClassroomKind::Guest)?;
        let now = (self.clock)() / 1000;
        let existing: Option<(Option<String>, bool)> = self.local.query_row(
            "SELECT r.entry_id,
                    EXISTS(SELECT 1 FROM acl_members m WHERE m.via_entry=r.entry_id AND m.member=r.applicant) AS joined
             FROM classroom_access_requests r
             LEFT JOIN classroom_entries c ON c.entry_id=r.entry_id AND c.recipient=r.applicant
             LEFT JOIN share_entries s ON s.entry_id=c.entry_id
             WHERE r.applicant=?1 AND r.scope_id=?2
               AND (r.entry_id IS NULL OR joined OR
                    (s.state='active' AND s.expires_at>?3 AND c.consumed_at IS NULL))
             ORDER BY joined DESC, (r.entry_id IS NOT NULL) DESC, r.created_at DESC
             LIMIT 1",
            params![caller.principal, surface_id, now],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional().map_err(db_err)?;
        if let Some((entry, joined)) = existing {
            return Ok(match (entry, joined) {
                (_, true) => json!({"ok":true,"state":"joined"}),
                (Some(entry), false) => json!({"ok":true,"state":"ready","entry_id":self.entry_public_id(&entry)}),
                (None, false) => json!({"ok":true,"state":"pending"}),
            });
        }
        self.acl_txn(caller, |tx| {
            let pending: i64 = tx.query_row(
                "SELECT count(*) FROM classroom_access_requests WHERE entry_id IS NULL", [], |row| row.get(0),
            ).map_err(db_err)?;
            if pending >= MAX_OPEN {
                return Err(WsError::limit("too many pending classroom requests"));
            }
            tx.execute(
                "INSERT INTO classroom_access_requests (request_id,scope_id,applicant,created_at)
                 VALUES (?1,?2,?3,?4)",
                params![random_id("ar_"), surface_id, caller.principal, now],
            ).map_err(db_err)?;
            Ok(((), vec![AuditItem {
                action:"classroom.request", target:json!({"entity_id":surface_id}), before:None, after:None,
            }]))
        })?;
        Ok(json!({"ok":true,"state":"pending"}))
    }

    pub fn classroom_requests(&self, teacher: &Caller) -> WsResult<Value> {
        self.require_classroom_owner(teacher)?;
        let now = (self.clock)() / 1000;
        let mut stmt = self.local.prepare_cached(
            "SELECT r.request_id,r.scope_id,r.applicant,r.created_at,r.entry_id,
                    EXISTS(SELECT 1 FROM acl_members m WHERE m.via_entry=r.entry_id AND m.member=r.applicant),
                    COALESCE(s.state='active' AND s.expires_at>?1 AND c.consumed_at IS NULL, 0) AS ready
             FROM classroom_access_requests r
             LEFT JOIN classroom_entries c ON c.entry_id=r.entry_id
             LEFT JOIN share_entries s ON s.entry_id=c.entry_id
             ORDER BY (r.entry_id IS NOT NULL), ready DESC, r.created_at DESC, r.request_id LIMIT 256",
        ).map_err(db_err)?;
        let requests = stmt
            .query_map([now], |row| {
                let entry: Option<String> = row.get(4)?;
                let state = if entry.is_none() {
                    "pending"
                } else if row.get::<_, bool>(5)? {
                    "approved"
                } else if row.get::<_, bool>(6)? {
                    "ready"
                } else {
                    "revoked"
                };
                Ok(
                    json!({"request_id":row.get::<_,String>(0)?,"scope_id":row.get::<_,String>(1)?,
                      "applicant":row.get::<_,String>(2)?,"created_at":row.get::<_,i64>(3)?,
                      "state":state,"entry_id":entry.map(|local| self.entry_public_id(&local))}),
                )
            })
            .map_err(db_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db_err)?;
        let mut stmt = self.local.prepare_cached(
            "SELECT c.entry_id,c.scope_id,c.recipient,c.kind,c.consumed_at,s.state,s.expires_at,
                    EXISTS(SELECT 1 FROM acl_members m WHERE m.via_entry=c.entry_id AND m.member=c.recipient)
             FROM classroom_entries c JOIN share_entries s ON s.entry_id=c.entry_id
             WHERE NOT EXISTS(SELECT 1 FROM classroom_access_requests r WHERE r.entry_id=c.entry_id)
             ORDER BY s.created_at DESC,c.entry_id LIMIT 256",
        ).map_err(db_err)?;
        let invitations = stmt
            .query_map([], |row| {
                let consumed: Option<i64> = row.get(4)?;
                let state: String = row.get(5)?;
                let expires: Option<i64> = row.get(6)?;
                let state = if consumed.is_some() && row.get::<_, bool>(7)? {
                    "redeemed"
                } else if state == "active"
                    && consumed.is_none()
                    && expires.is_some_and(|at| at > now)
                {
                    "active"
                } else {
                    "closed"
                };
                Ok(
                    json!({"entry_id":self.entry_public_id(&row.get::<_,String>(0)?),
                      "scope_id":row.get::<_,String>(1)?,
                      "recipient":row.get::<_,String>(2)?,
                      "kind":row.get::<_,String>(3)?,"state":state}),
                )
            })
            .map_err(db_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db_err)?;
        Ok(json!({"ok":true,"requests":requests,"invitations":invitations}))
    }

    pub fn classroom_request_recipient(&self, teacher: &Caller, request: &str) -> WsResult<String> {
        self.require_classroom_owner(teacher)?;
        self.local.query_row(
            "SELECT applicant FROM classroom_access_requests WHERE request_id=?1 AND entry_id IS NULL",
            [request], |row| row.get(0),
        ).optional().map_err(db_err)?.ok_or_else(unavailable)
    }

    pub fn classroom_approve(
        &self,
        teacher: &Caller,
        request: &str,
        recipient: &str,
    ) -> WsResult<Value> {
        self.require_classroom_owner(teacher)?;
        let (scope, applicant): (String,String) = self.local.query_row(
            "SELECT scope_id,applicant FROM classroom_access_requests WHERE request_id=?1 AND entry_id IS NULL",
            [request], |row| Ok((row.get(0)?,row.get(1)?)),
        ).optional().map_err(db_err)?.ok_or_else(unavailable)?;
        if applicant != recipient || applicant == teacher.principal || !valid_recipient(recipient) {
            return Err(unavailable());
        }
        let kind = ClassroomKind::Guest;
        let target = self.classroom_target(&scope, kind)?;
        let local = random_id("se_");
        let group = random_id("classroom-");
        let offer = Offer {
            entry: &local,
            group: &group,
            recipient,
            scope: &scope,
            kind,
            roots: &target.roots,
        };
        self.acl_txn(teacher, |tx| {
            let updated = tx
                .execute(
                    "UPDATE classroom_access_requests SET entry_id=?1
                 WHERE request_id=?2 AND applicant=?3 AND scope_id=?4 AND entry_id IS NULL",
                    params![local, request, recipient, scope],
                )
                .map_err(db_err)?;
            if updated != 1 {
                return Err(unavailable());
            }
            self.insert_offer(tx, teacher, &offer)?;
            Ok((
                (),
                vec![AuditItem {
                    action: "classroom.approve",
                    target: json!({"entity_id":scope,"recipient":recipient}),
                    before: None,
                    after: None,
                }],
            ))
        })?;
        Ok(json!({"ok":true,"entry_id":self.entry_public_id(&local)}))
    }

    pub fn is_classroom_entry(&self, local: &str) -> WsResult<bool> {
        self.local
            .query_row(
                "SELECT 1 FROM classroom_entries WHERE entry_id=?1",
                [local],
                |_| Ok(()),
            )
            .optional()
            .map(|row| row.is_some())
            .map_err(db_err)
    }

    /// Caller proof comes from the server. Close and consume even if membership is later revoked.
    pub fn classroom_redeem(
        &self,
        caller: &Caller,
        local: &str,
    ) -> WsResult<(String, Option<String>)> {
        self.require_classroom_recipient(caller)?;
        let entry = self
            .entry(local)?
            .filter(|e| e.state == EntryState::Active)
            .ok_or_else(unavailable)?;
        let (scope, mode, recipient, backing_folder): (String,String,String,Option<String>) = self.local.query_row(
            "SELECT scope_id,kind,recipient,backing_folder FROM classroom_entries WHERE entry_id=?1 AND consumed_at IS NULL",
            [local], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
        ).optional().map_err(db_err)?.ok_or_else(unavailable)?;
        if recipient != caller.principal {
            return Err(unavailable());
        }
        let kind = match mode.as_str() {
            "guest" => ClassroomKind::Guest,
            "student" => ClassroomKind::Student,
            _ => return Err(unavailable()),
        };
        let target = self.classroom_target(&scope, kind)?;
        if target.roots.get(1) != backing_folder.as_ref() {
            return Err(unavailable());
        }
        let now = self.now();
        let seconds = (self.clock)() / 1000;
        self.acl_txn(caller, |tx| {
            let closed = tx.execute(
                "UPDATE share_entries SET state='closed' WHERE entry_id=?1 AND state='active' AND expires_at>?2",
                params![local, seconds],
            ).map_err(db_err)?;
            let consumed = tx.execute(
                "UPDATE classroom_entries SET consumed_at=?1 WHERE entry_id=?2 AND recipient=?3 AND consumed_at IS NULL",
                params![seconds, local, recipient],
            ).map_err(db_err)?;
            if closed != 1 || consumed != 1 { return Err(unavailable()); }
            tx.execute(
                "INSERT INTO acl_members (group_id,member,via_entry,added_by,added_at) VALUES (?1,?2,?3,?2,?4)",
                params![entry.group_id, recipient, local, now],
            ).map_err(db_err)?;
            Ok(((), vec![AuditItem { action:"classroom.redeem", target:json!({"entity_id":scope,"kind":mode}), before:None, after:None }]))
        })?;
        Ok((scope, target.surface_title))
    }

    pub fn classroom_revoke(&self, teacher: &Caller, local: &str) -> WsResult<Value> {
        self.require_classroom_owner(teacher)?;
        let scope: String = self
            .local
            .query_row(
                "SELECT scope_id FROM classroom_entries WHERE entry_id=?1",
                [local],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?
            .ok_or_else(unavailable)?;
        self.acl_txn(teacher, |tx| {
            tx.execute(
                "UPDATE share_entries SET state='closed' WHERE entry_id=?1",
                [local],
            )
            .map_err(db_err)?;
            tx.execute("DELETE FROM acl_members WHERE via_entry=?1", [local])
                .map_err(db_err)?;
            Ok((
                (),
                vec![AuditItem {
                    action: "classroom.revoke",
                    target: json!({"entity_id":scope}),
                    before: None,
                    after: None,
                }],
            ))
        })?;
        Ok(json!({"ok":true}))
    }
}
