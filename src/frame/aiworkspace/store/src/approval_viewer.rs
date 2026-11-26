use crate::approval_capture::{approved_content_folder, is_container, live_shared, local_block_source, title};
use crate::docdb::SqlCtx;
use crate::objects::{Availability, ObjectStore};
use crate::workspace::Workspace;
use aiworkspace_core::model::{
    EntityRow, JsonMap, MemStore, ReadCtx, RichTextState, TreeEdge, CANVAS_CONTENT_ID, ROOT_ID, SURFACES_ID,
    TYPE_ANNOTATION, TYPE_ASSET, TYPE_CELL, TYPE_RICHTEXT, TYPE_TABLE,
};
use aiworkspace_core::order_key::{base_row_key, resolve_row_key};
use aiworkspace_core::replica::dump_rows;
use aiworkspace_core::value::FieldType;
use aiworkspace_core::{richtext, Code, WsError, WsResult};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

const MAX_ITEMS: usize = 512;
const MAX_DEPTH: usize = 24;
const MAX_RECORDS: usize = 2048;
const MAX_RECORD_ROWS: usize = 8192;
const MAX_FIELDS: usize = 256;
const MAX_COMPONENT_BYTES: usize = 6 * 1024 * 1024;
const MAX_ASSETS: usize = 128;
const MAX_ASSET_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ASSET_TOTAL: u64 = 128 * 1024 * 1024;

const OMIT_BLOCKS: &str = "Unsupported or interactive blocks were omitted.";
const OMIT_REFERENCES: &str = "Unavailable, external, or private references were omitted.";
const OMIT_NOTES: &str = "Private content, comments, and speaker notes are outside this preview.";
const OMIT_ASSETS: &str = "Unavailable or oversized attachments were omitted.";

#[derive(Debug, Clone, Serialize)]
pub struct ApprovalViewerAsset {
    pub object_id: String,
    pub name: String,
    pub media_type: String,
    pub size: u64,
}

impl ApprovalViewerAsset {
    pub fn read(&self, objects: &ObjectStore) -> WsResult<Vec<u8>> {
        if self.size > MAX_ASSET_BYTES || objects.file_size(&self.object_id)? != self.size {
            return Err(WsError::not_found("approved attachment not available"));
        }
        let data = objects.file_head(&self.object_id, self.size + 1)?;
        if data.len() as u64 != self.size {
            return Err(WsError::not_found("approved attachment not available"));
        }
        Ok(data)
    }
}

#[derive(Debug, Serialize)]
pub struct ApprovalViewer {
    pub work_log_cutoff: crate::approval_worklog::WorkLogCutoff,
    pub title: String,
    pub mode: &'static str,
    pub epoch: String,
    pub head_seq: u64,
    pub captured_at: String,
    pub rows: Value,
    pub assets: Vec<ApprovalViewerAsset>,
    pub assets_complete: bool,
    pub omissions: Vec<&'static str>,
}

fn limit() -> WsError {
    WsError::limit("canvas exceeds approval viewer capture limits")
}

fn select_props(source: &JsonMap, keys: &[&str]) -> JsonMap {
    keys.iter()
        .filter_map(|key| source.get(*key).map(|value| (key.to_string(), value.clone())))
        .collect()
}

fn passive_value(value: &Value, depth: usize) -> Option<Value> {
    if depth > MAX_DEPTH {
        return None;
    }
    Some(match value {
        Value::Object(fields) => {
            let safe = |key: &str| {
                !matches!(
                    key,
                    "url"
                        | "href"
                        | "src"
                        | "script"
                        | "js"
                        | "html"
                        | "css"
                        | "source_url"
                        | "object_id"
                        | "entity_id"
                        | "workspace_id"
                        | "zone"
                        | "ref"
                        | "__proto__"
                        | "constructor"
                        | "prototype"
                )
            };
            Value::Object(
                fields
                    .iter()
                    .filter(|(key, _)| safe(key))
                    .filter_map(|(key, value)| passive_value(value, depth + 1).map(|value| (key.clone(), value)))
                    .collect(),
            )
        }
        Value::Array(values) => Value::Array(
            values
                .iter()
                .filter_map(|value| passive_value(value, depth + 1))
                .collect(),
        ),
        Value::String(text) => {
            let lower = text.to_ascii_lowercase();
            if ["url(", "://", "javascript:", "data:", "@import", "image-set("]
                .iter()
                .any(|v| lower.contains(v))
            {
                return None;
            }
            value.clone()
        }
        _ => value.clone(),
    })
}

fn insert_entity(store: &mut MemStore, mut entity: EntityRow, mut edge: TreeEdge, parent: Option<&str>) {
    entity.derived = None;
    entity.created_by = None;
    if let Some(parent) = parent {
        edge.parent_id = parent.to_owned();
    }
    store.edges.insert(entity.entity_id.clone(), edge);
    store.entities.insert(entity.entity_id.clone(), entity);
}

fn safe_media_type(media: &str) -> &str {
    if matches!(
        media,
        "image/png"
            | "image/jpeg"
            | "image/gif"
            | "image/webp"
            | "image/avif"
            | "image/bmp"
            | "application/pdf"
            | "text/plain"
            | "audio/ogg"
            | "audio/mpeg"
            | "audio/wav"
            | "video/mp4"
            | "video/webm"
    ) {
        media
    } else {
        "application/octet-stream"
    }
}

fn file_name(entity: &EntityRow) -> String {
    let source = entity
        .payload
        .get("file_name")
        .and_then(Value::as_str)
        .or(entity.name.as_deref())
        .unwrap_or("attachment");
    let clean: String = source
        .chars()
        .take(120)
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '"' | '<' | '>' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    if clean.trim_matches([' ', '.']).is_empty() {
        "attachment".into()
    } else {
        clean
    }
}

fn local_ref_type(view: &str) -> Option<&'static str> {
    match view {
        "richtext" => Some(TYPE_RICHTEXT),
        "asset" => Some(TYPE_ASSET),
        "table" => Some(TYPE_TABLE),
        "note" => Some(TYPE_ANNOTATION),
        _ => None,
    }
}

struct Selection<'a> {
    ctx: &'a SqlCtx<'a>,
    objects: &'a ObjectStore,
    mem: MemStore,
    assets: BTreeMap<String, ApprovalViewerAsset>,
    assets_size: u64,
    payload_bytes: usize,
    omissions: BTreeSet<&'static str>,
    folder: Option<String>,
}

impl Selection<'_> {
    fn add_asset(&mut self, object_id: &str, name: &str) -> WsResult<bool> {
        if self.assets.contains_key(object_id) {
            return Ok(true);
        }
        if !aiworkspace_core::canonical::is_obj_id(object_id) {
            self.omissions.insert(OMIT_ASSETS);
            return Ok(false);
        }
        let Some(mut info) = self.ctx.asset(object_id)? else {
            self.omissions.insert(OMIT_ASSETS);
            return Ok(false);
        };
        if self.assets.len() >= MAX_ASSETS
            || info.size > MAX_ASSET_BYTES
            || self.assets_size + info.size > MAX_ASSET_TOTAL
            || self.objects.availability(object_id) != Availability::Available
        {
            self.omissions.insert(OMIT_ASSETS);
            return Ok(false);
        }
        self.assets_size += info.size;
        info.media_type = safe_media_type(&info.media_type).to_string();
        self.assets.insert(
            object_id.to_owned(),
            ApprovalViewerAsset {
                object_id: object_id.to_owned(),
                name: name.to_owned(),
                media_type: info.media_type.clone(),
                size: info.size,
            },
        );
        self.mem.assets.insert(object_id.to_owned(), info);
        Ok(true)
    }

    fn add_source(&mut self, mut source: EntityRow) -> WsResult<bool> {
        if self.mem.entities.contains_key(&source.entity_id) {
            return Ok(true);
        }
        let Some(folder) = self.folder.clone() else {
            return Ok(false);
        };
        source.payload = match source.type_id.as_str() {
            TYPE_ANNOTATION
                if source.payload.get("kind").and_then(Value::as_str) == Some("note")
                    && source.payload.get("target").is_none_or(Value::is_null) =>
            {
                select_props(&source.payload, &["kind", "body", "title"])
            }
            TYPE_RICHTEXT => select_props(&source.payload, &["title"]),
            TYPE_TABLE if source.payload.get("data_mode").is_none_or(|mode| mode != "url_query") => {
                select_props(&source.payload, &["title", "title_field_id"])
            }
            TYPE_ASSET => {
                let name = file_name(&source);
                let mut payload = select_props(&source.payload, &["title", "media_type", "size"]);
                payload.insert("file_name".into(), json!(name));
                let Some(id) = source.payload.get("object_id").and_then(Value::as_str) else {
                    self.omissions.insert(OMIT_ASSETS);
                    return Ok(false);
                };
                if !self.add_asset(id, &name)? {
                    return Ok(false);
                }
                payload.insert("object_id".into(), json!(id));
                payload.insert("media_type".into(), json!(self.assets[id].media_type));
                if let Some(poster) = source.payload.get("poster").and_then(Value::as_str) {
                    if self.add_asset(poster, &name)? {
                        payload.insert("poster".into(), json!(poster));
                    }
                }
                source.name = Some(name);
                payload
            }
            _ => return Ok(false),
        };
        let Some(edge) = self.ctx.edge(&source.entity_id)? else {
            return Ok(false);
        };
        self.add_entity(source, edge, Some(&folder))?;
        Ok(true)
    }

    fn add_entity(&mut self, entity: EntityRow, edge: TreeEdge, parent: Option<&str>) -> WsResult<()> {
        self.payload_bytes += serde_json::to_vec(&entity.payload).map_err(|_| limit())?.len();
        if self.payload_bytes > MAX_COMPONENT_BYTES {
            return Err(limit());
        }
        insert_entity(&mut self.mem, entity, edge, parent);
        Ok(())
    }

    fn cell_payload(&mut self, cell: &EntityRow) -> WsResult<Option<JsonMap>> {
        let Some(view) = cell
            .payload
            .get("view")
            .and_then(|view| view.get("type"))
            .and_then(Value::as_str)
        else {
            return Ok(None);
        };
        if !matches!(
            view,
            "label" | "shape" | "frame" | "connector" | "ink" | "richtext" | "table" | "asset" | "note"
        ) || cell.payload.get("def_ref").is_some_and(|v| !v.is_null())
            || cell
                .payload
                .get("view")
                .and_then(|view| view.get("version"))
                .is_some_and(|v| v.as_u64() != Some(1))
        {
            return Ok(None);
        }
        let mut safe = select_props(&cell.payload, &["title"]);
        safe.insert("view".into(), json!({ "type": view, "version": 1 }));
        for key in [
            "config",
            "fields",
            "filter",
            "sorts",
            "group",
            "manual_order",
            "options",
            "locked",
            "flip",
            "start",
            "end",
            "route",
            "controls",
            "label",
            "slot",
            "row_limit",
        ] {
            if let Some(value) = cell.payload.get(key).and_then(|value| passive_value(value, 0)) {
                safe.insert(key.into(), value);
            }
        }
        if let Some(presentation) = cell.payload.get("presentation").and_then(Value::as_object) {
            let mut projected = select_props(presentation, &["caption"]);
            if let Some(bg) = presentation.get("background").and_then(|value| passive_value(value, 0)) {
                projected.insert("background".into(), bg);
            }
            safe.insert("presentation".into(), Value::Object(projected));
        }
        if view == "table"
            && ["fields", "filter", "row_limit"].iter().any(|key| {
                cell.payload
                    .get(*key)
                    .is_some_and(|value| !value.is_null() && !value.as_array().is_some_and(Vec::is_empty))
            })
        {
            self.omissions.insert(OMIT_REFERENCES);
            return Ok(None);
        }
        if let Some(expected) = local_ref_type(view) {
            if let Some(source) =
                local_block_source(self.ctx, cell, self.folder.as_deref())?.filter(|source| source.type_id == expected)
            {
                let source_id = source.entity_id.clone();
                if self.add_source(source)? {
                    safe.insert(
                        "source_ref".into(),
                        json!({ "entity_id": source_id, "version": { "mode": "live_head" } }),
                    );
                    return Ok(Some(safe));
                }
            }
            self.omissions.insert(OMIT_REFERENCES);
            return Ok(None);
        }
        Ok(Some(safe))
    }

    // The deployed review viewer uses the earlier replica format. Project ranks only
    // after table rows have passed this Surface's existing selection and capture limits.
    fn project_table_order(&mut self) -> WsResult<()> {
        for (cell_id, cell) in &mut self.mem.entities {
            if cell.type_id != TYPE_CELL || cell.payload["view"]["type"] != "table" {
                continue;
            }
            let Some(source) = cell.payload["source_ref"]["entity_id"].as_str() else { continue };
            let Some(original) = self.ctx.entity(cell_id)? else { continue };
            let Some(row_order) = original.payload.get("row_order").and_then(Value::as_object) else { continue };
            let rows: BTreeMap<_, _> = self.mem.records
                .range((source.to_string(), String::new())..)
                .take_while(|((table, _), _)| table == source)
                .map(|(_, row)| (row.record_id.as_str(), row.created_seq))
                .collect();
            let mut ordered = Vec::with_capacity(rows.len());
            for (&id, &sequence) in &rows {
                let rank = match row_order.get(id).and_then(Value::as_str) {
                    Some(rank) => resolve_row_key(rank, |anchor| Ok(rows.get(anchor).copied()))?,
                    None => None,
                };
                ordered.push((rank.unwrap_or_else(|| base_row_key(sequence, id)), id));
            }
            ordered.sort_unstable();
            let projected = Value::Object(ordered.into_iter().enumerate()
                .map(|(index, (_, id))| (id.to_string(), json!(format!("{index:04x}h"))))
                .collect());
            self.payload_bytes += serde_json::to_vec(&projected).map_err(|_| limit())?.len();
            if self.payload_bytes > MAX_COMPONENT_BYTES {
                return Err(limit());
            }
            cell.payload.insert("manual_order".into(), projected);
        }
        Ok(())
    }

    fn retain_connector_anchors(&mut self) -> WsResult<()> {
        let members: BTreeSet<_> = self
            .mem
            .entities
            .values()
            .filter(|entity| entity.type_id == TYPE_CELL || is_container(entity, "group"))
            .map(|entity| entity.entity_id.clone())
            .collect();
        for entity in self.mem.entities.values_mut() {
            if entity.payload.get("view").and_then(|v| v["type"].as_str()) != Some("connector") {
                continue;
            }
            let Some(original) = self.ctx.entity(&entity.entity_id)? else {
                continue;
            };
            for key in ["start", "end"] {
                let target = original.payload.get(key).and_then(|value| value["entity_id"].as_str());
                if let Some(anchor) = original
                    .payload
                    .get(key)
                    .filter(|_| target.is_some_and(|id| members.contains(id)))
                {
                    entity.payload.insert(key.into(), anchor.clone());
                } else {
                    entity.payload.remove(key);
                    if target.is_some() {
                        self.omissions.insert(OMIT_REFERENCES);
                    }
                }
            }
        }
        Ok(())
    }

    fn walk(&mut self, root_id: &str) -> WsResult<()> {
        let mut pending = vec![(root_id.to_string(), 0)];
        let mut checked = 0;
        while let Some((parent, depth)) = pending.pop() {
            if depth >= MAX_DEPTH {
                return Err(limit());
            }
            let remaining = MAX_ITEMS - checked;
            let children = self.ctx.children_limited(&parent, remaining as i64 + 1)?;
            if children.len() > remaining {
                return Err(limit());
            }
            for edge in children {
                checked += 1;
                let Some(mut entity) = self.ctx.entity(&edge.child_id)?.filter(live_shared) else {
                    continue;
                };
                let payload = if entity.type_id == TYPE_CELL {
                    self.cell_payload(&entity)?
                } else if is_container(&entity, "group") || is_container(&entity, "pdf-frame") {
                    let mut p = select_props(&entity.payload, &["kind", "title", "slot"]);
                    if let Some(config) = entity.payload.get("config").and_then(|v| passive_value(v, 0)) {
                        p.insert("config".into(), config);
                    }
                    Some(p)
                } else {
                    None
                };
                let Some(payload) = payload else {
                    self.omissions.insert(OMIT_BLOCKS);
                    continue;
                };
                pending.push((entity.entity_id.clone(), depth + 1));
                entity.payload = payload;
                self.add_entity(entity, edge, None)?;
            }
        }
        Ok(())
    }
}

fn strip_richtext_refs(node: &mut Value, changed: &mut bool) {
    if let Some(marks) = node.get_mut("marks").and_then(Value::as_array_mut) {
        let before = marks.len();
        marks.retain(|mark| mark["type"] != "link");
        *changed |= marks.len() != before;
    }
    if let Some(content) = node.get_mut("content").and_then(Value::as_array_mut) {
        for child in content.iter_mut() {
            match child["type"].as_str() {
                Some("object_link") => {
                    *changed = true;
                    let label = child["attrs"]["label"]
                        .as_str()
                        .filter(|label| !label.is_empty())
                        .unwrap_or("link");
                    *child = json!({ "type": "text", "text": label });
                }
                Some("object_embed") => {
                    *changed = true;
                    let id = child["attrs"]["block_id"].as_str().unwrap_or("omitted-object");
                    *child = json!({ "type": "paragraph", "attrs": { "block_id": id } });
                }
                _ => strip_richtext_refs(child, changed),
            }
        }
    }
}

impl Workspace {
    fn select_approval_viewer<'a>(
        &'a self,
        ctx: &'a SqlCtx<'a>,
        owner: &str,
        surface_id: &str,
    ) -> WsResult<(Selection<'a>, String, &'static str)> {
        let mut surface = self.approval_surface(ctx, owner, surface_id)?;
        let title = title(&surface)?;
        let mode = if surface.payload.get("layout").and_then(|v| v["mode"].as_str()) == Some("free") {
            "free"
        } else {
            "flow"
        };
        let folder_id = approved_content_folder(ctx, &surface)?;
        let mut scope = Selection {
            ctx,
            objects: &self.objects,
            mem: MemStore::with_root(),
            assets: BTreeMap::new(),
            assets_size: 0,
            payload_bytes: 0,
            omissions: BTreeSet::from([OMIT_NOTES]),
            folder: folder_id.clone(),
        };
        let mut p = select_props(&surface.payload, &["kind", "title"]);
        p.insert("layout".into(), json!({ "mode": mode }));
        if let Some(config) = surface.payload.get("config").and_then(|v| passive_value(v, 0)) {
            p.insert("config".into(), config);
        }
        if let Some(folder_id) = folder_id {
            if let (Some(mut folder), Some(edge)) = (ctx.entity(&folder_id)?, ctx.edge(&folder_id)?) {
                folder.payload = select_props(&folder.payload, &["kind", "system", "surface_id"]);
                folder.name = None;
                scope.add_entity(folder, edge, Some(CANVAS_CONTENT_ID))?;
                p.insert("content_folder_id".into(), json!(folder_id));
            }
        }
        if let Some(fonts) = self.settings()?.0["fonts"]
            .as_object()
            .and_then(|fonts| passive_value(&Value::Object(select_props(fonts, &["sans", "serif", "mono"])), 0))
        {
            if let Some(root) = scope.mem.entities.get_mut(ROOT_ID) {
                root.payload.insert("config".into(), json!({ "fonts": fonts }));
            }
        }
        surface.payload = p;
        let edge = ctx
            .edge(surface_id)?
            .ok_or_else(|| WsError::not_found("approved canvas not found"))?;
        scope.add_entity(surface, edge, Some(SURFACES_ID))?;
        scope.walk(surface_id)?;
        scope.retain_connector_anchors()?;
        Ok((scope, title, mode))
    }

    pub fn capture_approval_viewer(&self, owner: &str, surface_id: &str) -> WsResult<ApprovalViewer> {
        let now = self.now();
        let ctx = self.ctx(&now);
        let (mut scope, title, mode) = self.select_approval_viewer(&ctx, owner, surface_id)?;
        let mut field_count = 0usize;
        let mut record_count = 0usize;
        let mut record_rows = 0usize;
        for (id, source) in &scope.mem.entities {
            if source.type_id == TYPE_RICHTEXT {
                let Some(state) = ctx.richtext(id)? else { continue };
                let mut ast = state.meta.ast;
                let mut removed = false;
                strip_richtext_refs(&mut ast, &mut removed);
                if removed {
                    scope.omissions.insert(OMIT_REFERENCES);
                }
                let ast = richtext::canonicalize(&ast)?;
                scope.payload_bytes += serde_json::to_vec(&ast).map_err(|_| limit())?.len();
                if scope.payload_bytes > MAX_COMPONENT_BYTES {
                    return Err(limit());
                }
                let validated = richtext::validate_ast(&ast, &richtext::Limits::default())?;
                let doc = richtext::build_doc(&ast, 1)?;
                let meta = aiworkspace_core::model::RichTextMeta {
                    ast,
                    block_index: richtext::initial_block_index(&validated, self.head_seq),
                    ..state.meta
                };
                scope.mem.richtexts.insert(id.clone(), RichTextState { meta, doc });
            } else if source.type_id == TYPE_TABLE {
                let mut allowed = BTreeSet::new();
                for mut field in ctx.fields(id)?.into_iter().filter(|field| field.alive()) {
                    if field.def.submitter || matches!(field.def.ty, FieldType::ObjectRef | FieldType::Json) {
                        scope.omissions.insert(OMIT_REFERENCES);
                        continue;
                    }
                    field.def.default = None;
                    field_count += 1;
                    if field_count > MAX_FIELDS {
                        return Err(limit());
                    }
                    allowed.insert(field.field_id.clone());
                    scope.mem.fields.insert((id.clone(), field.field_id.clone()), field);
                }
                let mut put_record = |record: &aiworkspace_core::model::RecordRow| {
                    record_rows += 1;
                    if record_rows > MAX_RECORD_ROWS {
                        return Err(limit());
                    }
                    if !record.alive() {
                        return Ok(true);
                    }
                    record_count += 1;
                    if record_count > MAX_RECORDS {
                        return Err(limit());
                    }
                    let mut record = record.clone();
                    record.body_entity_id = None;
                    record.meta.clear();
                    let scalar = |v: &Value| v.is_null() || v.is_boolean() || v.is_number() || v.is_string();
                    record.values.retain(|k, v| {
                        allowed.contains(k) && (scalar(v) || v.as_array().is_some_and(|list| list.iter().all(scalar)))
                    });
                    record.revs.retain(|k, _| allowed.contains(k));
                    scope.payload_bytes += serde_json::to_vec(&record.values).map_err(|_| limit())?.len();
                    if scope.payload_bytes > MAX_COMPONENT_BYTES {
                        return Err(limit());
                    }
                    scope.mem.records.insert((id.clone(), record.record_id.clone()), record);
                    Ok(true)
                };
                ctx.scan_records(id, &mut put_record)?;
            }
        }
        scope.project_table_order()?;
        Ok(ApprovalViewer {
            work_log_cutoff: self.approval_work_log_cutoff(surface_id)?,
            title,
            mode,
            epoch: self.epoch.clone(),
            head_seq: self.head_seq,
            captured_at: now.clone(),
            rows: dump_rows(&scope.mem)?,
            assets: scope.assets.into_values().collect(),
            assets_complete: !scope.omissions.contains(OMIT_ASSETS),
            omissions: scope.omissions.into_iter().collect(),
        })
    }

    pub fn find_approval_viewer_asset(
        &self,
        owner: &str,
        surface_id: &str,
        object_id: &str,
        version: Option<(&str, u64)>,
    ) -> WsResult<ApprovalViewerAsset> {
        let now = self.now();
        let ctx = self.ctx(&now);
        let (mut scope, _, _) = self.select_approval_viewer(&ctx, owner, surface_id)?;
        if version.is_some_and(|(epoch, head_seq)| epoch != self.epoch || head_seq != self.head_seq) {
            return Err(WsError::new(Code::RevisionConflict, "approved canvas version changed"));
        }
        scope
            .assets
            .remove(object_id)
            .ok_or_else(|| WsError::not_found("approved attachment not found"))
    }
}
