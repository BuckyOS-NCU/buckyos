use crate::docdb::{meta_get, SqlCtx};
use crate::workspace::Workspace;
use aiworkspace_core::model::{
    EntityRow, ReadCtx, TreeEdge, CANVAS_CONTENT_ID, SCOPE_SHARED, SURFACES_ID, TYPE_ANNOTATION, TYPE_CELL,
    TYPE_CONTAINER, TYPE_RICHTEXT,
};
use aiworkspace_core::value::{reference_entity_id, reference_is_local};
use aiworkspace_core::{WsError, WsResult};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;

const MAX_NODES: usize = 512;
const MAX_DEPTH: usize = 24;
const MAX_TITLE_BYTES: usize = 1024;
const MAX_TEXT_BYTES: usize = 16 * 1024;
const MAX_CANVAS_TEXT_BYTES: usize = 512 * 1024;
const MAX_COORDINATE: f64 = 1_000_000.0;
const OMIT_UNSUPPORTED: &str = "Some blocks, attachments, or dynamic views are outside this text and layout capture.";
const OMIT_REFERENCE: &str = "Non-local, private, historical, or otherwise unavailable block sources were omitted.";
const OMIT_GEOMETRY: &str = "Some positioned items are represented in reading order.";
const OMIT_COMMENTS: &str = "Comment threads, speaker notes, and live media are outside this capture.";

#[derive(Debug, Serialize)]
pub struct ApprovalBlock {
    pub id: String,
    pub kind: &'static str,
    pub title: String,
    pub text: String,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub layout: Option<ApprovalLayout>,
    pub depth: usize,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ApprovalLayout {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Serialize)]
pub struct ApprovalCanvas {
    pub title: String,
    pub blocks: Vec<ApprovalBlock>,
}

#[derive(Debug, Serialize)]
pub struct ApprovalSurfaceCapture {
    pub epoch: String,
    pub head_seq: u64,
    pub captured_at: String,
    pub canvas: ApprovalCanvas,
    pub omissions: Vec<&'static str>,
}

fn unavailable() -> WsError {
    WsError::not_found("approved canvas not found")
}

fn capture_limit() -> WsError {
    WsError::limit("canvas is too large for an approval capture")
}

pub(super) fn live_shared(entity: &EntityRow) -> bool {
    entity.alive() && entity.scope == SCOPE_SHARED && !entity.opaque()
}

pub(super) fn is_container(entity: &EntityRow, kind: &str) -> bool {
    entity.type_id == TYPE_CONTAINER && entity.payload.get("kind").and_then(Value::as_str) == Some(kind)
}

pub(super) fn title(entity: &EntityRow) -> WsResult<String> {
    let title = entity
        .payload
        .get("title")
        .and_then(Value::as_str)
        .or(entity.name.as_deref())
        .unwrap_or_default();
    if title.len() > MAX_TITLE_BYTES {
        return Err(capture_limit());
    }
    Ok(title.to_string())
}

pub(super) fn shared_descendant(ctx: &SqlCtx<'_>, entity_id: &str, root_id: &str) -> WsResult<bool> {
    let mut current = entity_id.to_string();
    for _ in 0..MAX_DEPTH {
        let Some(edge) = ctx.edge(&current)? else {
            return Ok(false);
        };
        if edge.parent_id == root_id {
            return Ok(true);
        }
        let Some(parent) = ctx.entity(&edge.parent_id)?.filter(live_shared) else {
            return Ok(false);
        };
        if !is_container(&parent, "folder") {
            return Ok(false);
        }
        current = parent.entity_id;
    }
    Ok(false)
}

pub(super) fn approved_content_folder(ctx: &SqlCtx<'_>, surface: &EntityRow) -> WsResult<Option<String>> {
    let Some(folder_id) = surface.payload.get("content_folder_id").and_then(Value::as_str) else {
        return Ok(None);
    };
    let Some(folder) = ctx.entity(folder_id)?.filter(live_shared) else {
        return Ok(None);
    };
    let owned = is_container(&folder, "folder")
        && folder.payload.get("system").and_then(Value::as_str) == Some("surface_content")
        && folder.payload.get("surface_id").and_then(Value::as_str) == Some(surface.entity_id.as_str())
        && ctx
            .edge(folder_id)?
            .is_some_and(|edge| edge.parent_id == CANVAS_CONTENT_ID);
    Ok(owned.then_some(folder.entity_id))
}

pub(super) fn local_block_source(
    ctx: &SqlCtx<'_>,
    cell: &EntityRow,
    folder: Option<&str>,
) -> WsResult<Option<EntityRow>> {
    let (Some(reference), Some(folder)) = (cell.payload.get("source_ref"), folder) else {
        return Ok(None);
    };
    let live_head = reference
        .get("version")
        .is_none_or(|v| v.get("mode").and_then(Value::as_str) == Some("live_head"));
    if !reference_is_local(reference)
        || reference.get("zone").is_some()
        || reference.get("selector").is_some()
        || reference
            .get("mode")
            .is_some_and(|value| value.as_str() != Some("live"))
        || !live_head
    {
        return Ok(None);
    }
    let Some(id) = reference_entity_id(reference) else {
        return Ok(None);
    };
    if !shared_descendant(ctx, id, folder)? {
        return Ok(None);
    }
    Ok(ctx
        .entity(id)?
        .filter(|source| live_shared(source) && source.payload.get("source").is_none_or(Value::is_null)))
}

fn offset(edge: &TreeEdge, parent: Option<(f64, f64)>) -> Option<(f64, f64)> {
    let (px, py) = parent?;
    let p = edge.placement.as_ref()?;
    let x = p["x"].as_f64()? + px;
    let y = p["y"].as_f64()? + py;
    (x.abs() <= MAX_COORDINATE && y.abs() <= MAX_COORDINATE).then_some((x, y))
}

fn layout(edge: &TreeEdge, origin: Option<(f64, f64)>) -> Option<ApprovalLayout> {
    let (x, y) = origin?;
    let p = edge.placement.as_ref()?;
    let (w, h) = (p["w"].as_f64()?, p["h"].as_f64()?);
    (0.0 < w && w <= MAX_COORDINATE && 0.0 < h && h <= MAX_COORDINATE).then_some(ApprovalLayout { x, y, w, h })
}

fn append_text(out: &mut String, value: &str) -> WsResult<()> {
    if out.len() + value.len() > MAX_TEXT_BYTES {
        return Err(capture_limit());
    }
    out.push_str(value);
    Ok(())
}

fn rich_text(ast: &Value, out: &mut String, depth: usize, omitted: &mut BTreeSet<&'static str>) -> WsResult<()> {
    if depth > MAX_DEPTH {
        return Err(capture_limit());
    }
    let kind = ast["type"].as_str().unwrap_or_default();
    match kind {
        "text" => return append_text(out, ast["text"].as_str().unwrap_or_default()),
        "hard_break" | "horizontal_rule" => return append_text(out, "\n"),
        "doc" | "paragraph" | "heading" | "blockquote" | "bullet_list" | "ordered_list" | "list_item"
        | "code_block" | "table" | "table_row" | "table_cell" => {}
        _ => {
            omitted.insert(OMIT_UNSUPPORTED);
            return Ok(());
        }
    }
    if let Some(content) = ast["content"].as_array() {
        if content.len() > MAX_TEXT_BYTES {
            return Err(capture_limit());
        }
        for child in content {
            rich_text(child, out, depth + 1, omitted)?;
        }
    }
    if matches!(kind, "paragraph" | "heading" | "code_block") && !out.is_empty() && !out.ends_with('\n') {
        append_text(out, "\n")?;
    }
    Ok(())
}

fn block_text(
    ctx: &SqlCtx<'_>,
    entity: &EntityRow,
    folder: Option<&str>,
    omitted: &mut BTreeSet<&'static str>,
) -> WsResult<Option<(&'static str, String)>> {
    if is_container(entity, "group") {
        return Ok(Some(("group", String::new())));
    }
    if is_container(entity, "pdf-frame") {
        return Ok(Some(("frame", String::new())));
    }
    if entity.type_id != TYPE_CELL {
        omitted.insert(OMIT_UNSUPPORTED);
        return Ok(None);
    }
    let kind = entity
        .payload
        .get("view")
        .and_then(|view| view["type"].as_str())
        .unwrap_or_default();
    let text = match kind {
        "frame" => return Ok(Some(("frame", String::new()))),
        "label" if !entity.payload.contains_key("source_ref") => return Ok(Some(("text", title(entity)?))),
        "note" => {
            let Some(note) = local_block_source(ctx, entity, folder)?.filter(|e| {
                e.type_id == TYPE_ANNOTATION
                    && e.payload.get("kind").and_then(Value::as_str) == Some("note")
                    && !e.payload.contains_key("target")
            }) else {
                omitted.insert(OMIT_REFERENCE);
                return Ok(None);
            };
            let body = note.payload.get("body").and_then(Value::as_str).unwrap_or_default();
            if body.len() > MAX_TEXT_BYTES {
                return Err(capture_limit());
            }
            return Ok(Some(("note", body.to_owned())));
        }
        "richtext" => {
            let Some(source) = local_block_source(ctx, entity, folder)?.filter(|e| e.type_id == TYPE_RICHTEXT) else {
                omitted.insert(OMIT_REFERENCE);
                return Ok(None);
            };
            let Some(source) = ctx.richtext(&source.entity_id)? else {
                omitted.insert(OMIT_REFERENCE);
                return Ok(None);
            };
            let mut text = String::new();
            rich_text(&source.meta.ast, &mut text, 0, omitted)?;
            text.trim_end_matches('\n').to_owned()
        }
        _ => {
            omitted.insert(OMIT_UNSUPPORTED);
            return Ok(None);
        }
    };
    Ok(Some(("text", text)))
}

impl Workspace {
    pub(super) fn approval_surface(&self, ctx: &SqlCtx<'_>, owner: &str, surface_id: &str) -> WsResult<EntityRow> {
        if self.owner()?.as_deref() != Some(owner) || meta_get(&self.doc, "purpose")?.is_some() {
            return Err(unavailable());
        }
        let Some(surface) = ctx
            .entity(surface_id)?
            .filter(|e| live_shared(e) && is_container(e, "surface"))
        else {
            return Err(unavailable());
        };
        if !shared_descendant(ctx, surface_id, SURFACES_ID)? {
            return Err(unavailable());
        }
        Ok(surface)
    }

    /// Capture text and placement from one shared Surface at the locked writer's current head.
    /// The HTTP boundary resolves `owner` from an existing verified school binding.
    pub fn capture_approval_surface(&self, owner: &str, surface_id: &str) -> WsResult<ApprovalSurfaceCapture> {
        let now = self.now();
        let ctx = self.ctx(&now);
        let surface = self.approval_surface(&ctx, owner, surface_id)?;
        let folder = approved_content_folder(&ctx, &surface)?;
        let mut omitted = BTreeSet::from([OMIT_COMMENTS]);
        let queue = |parent: &str, origin: Option<(f64, f64)>, depth: usize, remaining: usize| {
            if depth > MAX_DEPTH {
                return Err(capture_limit());
            }
            let children = ctx.children_limited(parent, remaining as i64 + 1)?;
            if children.len() > remaining {
                return Err(capture_limit());
            }
            Ok(children.into_iter().rev().map(move |edge| (edge, origin, depth)))
        };
        let mut pending: Vec<_> = queue(surface_id, Some((0.0, 0.0)), 0, MAX_NODES)?.collect();
        let mut visited = 0usize;
        let mut text_bytes = 0usize;
        let mut blocks = Vec::new();
        while let Some((edge, ancestor_origin, depth)) = pending.pop() {
            visited += 1;
            let Some(entity) = ctx.entity(&edge.child_id)?.filter(EntityRow::alive) else {
                continue;
            };
            if entity.scope != SCOPE_SHARED {
                continue;
            }
            if entity.opaque() {
                omitted.insert(OMIT_UNSUPPORTED);
                continue;
            }
            let Some((kind, text)) = block_text(&ctx, &entity, folder.as_deref(), &mut omitted)? else {
                continue;
            };
            let title = if entity.payload.get("view").and_then(|view| view["type"].as_str()) == Some("label") {
                String::new()
            } else {
                title(&entity)?
            };
            text_bytes += title.len() + text.len();
            if text_bytes > MAX_CANVAS_TEXT_BYTES {
                return Err(capture_limit());
            }
            let block_origin = offset(&edge, ancestor_origin);
            let block_layout = layout(&edge, block_origin);
            if edge.placement.is_some() && block_layout.is_none() {
                omitted.insert(OMIT_GEOMETRY);
            }
            if kind == "group" || is_container(&entity, "pdf-frame") {
                let remaining = MAX_NODES.saturating_sub(visited + pending.len());
                pending.extend(queue(&entity.entity_id, block_origin, depth + 1, remaining)?);
            }
            blocks.push(ApprovalBlock {
                id: entity.entity_id,
                kind,
                title,
                text,
                layout: block_layout,
                depth,
            });
        }
        Ok(ApprovalSurfaceCapture {
            epoch: self.epoch.clone(),
            head_seq: self.head_seq,
            captured_at: now,
            canvas: ApprovalCanvas {
                title: title(&surface)?,
                blocks,
            },
            omissions: omitted.into_iter().collect(),
        })
    }
}
