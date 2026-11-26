mod common;

use aiworkspace_core::Code;
use common::{alice, env, ok};
use serde_json::{json, Value};

fn capture_surface(id: &str, folder: &str, key: &str, title: &str) -> [Value; 2] {
    [
        json!({
            "op": "entity.create", "entity_id": folder, "type_id": "buckyos.container",
            "parent_id": "canvas-content", "order_key": key,
            "payload": { "kind": "folder", "system": "surface_content", "surface_id": id }
        }),
        json!({
            "op": "entity.create", "entity_id": id, "type_id": "buckyos.container",
            "parent_id": "surfaces", "order_key": key,
            "payload": { "kind": "surface", "layout": { "mode": "free" }, "title": title, "content_folder_id": folder }
        }),
    ]
}

fn note(id: &str, parent: &str, key: &str, body: &str, scope: &str) -> Value {
    json!({
        "op": "entity.create", "entity_id": id, "type_id": "buckyos.annotation",
        "parent_id": parent, "order_key": key, "scope": scope,
        "payload": { "kind": "note", "body": body }
    })
}

fn cell(id: &str, parent: &str, key: &str, payload: Value) -> Value {
    json!({
        "op": "entity.create", "entity_id": id, "type_id": "buckyos.cell",
        "parent_id": parent, "order_key": key, "payload": payload,
        "placement": { "x": 40, "y": -20, "w": 180, "h": 90 }
    })
}

#[test]
fn approval_capture_selects_shared_content_for_exact_owner_at_one_revision() {
    let env = env();
    let created = env.svc.create_workspace(&alice(), "Coursework", None).unwrap();
    let handle = env.svc.workspace(created["workspace_id"].as_str().unwrap()).unwrap();
    let mut ws = handle.lock().unwrap();
    let mut create = Vec::new();
    create.extend(capture_surface("selected", "selected-content", "a", "Approved Canvas"));
    create.extend(capture_surface("other", "other-content", "b", "Different Canvas"));
    create.extend([
        json!({
            "op": "entity.create", "entity_id": "document", "type_id": "buckyos.richtext",
            "parent_id": "selected-content", "order_key": "a",
            "payload": { "content": { "type": "doc", "content": [
                { "type": "paragraph", "attrs": { "block_id": "p1" },
                  "content": [{ "type": "text", "text": "Approved native rich text." }] }
            ] } }
        }),
        note(
            "selected-note",
            "selected-content",
            "b",
            "Approved sticky note.",
            "shared",
        ),
        note("other-note", "other-content", "a", "Other surface secret.", "shared"),
        note(
            "private-note",
            "selected-content",
            "c",
            "Private note secret.",
            "personal",
        ),
        cell(
            "frame",
            "selected",
            "a",
            json!({
                "view": { "type": "frame" }, "title": "Approved frame",
                "presentation": { "notes": "Speaker note secret." }
            }),
        ),
        cell(
            "rich",
            "selected",
            "b",
            json!({
                "view": { "type": "richtext" }, "source_ref": { "entity_id": "document" }
            }),
        ),
        cell(
            "sticky",
            "selected",
            "c",
            json!({
                "view": { "type": "note" }, "source_ref": { "entity_id": "selected-note" }
            }),
        ),
        cell(
            "outside-source",
            "selected",
            "d",
            json!({
                "view": { "type": "note" }, "source_ref": { "entity_id": "other-note" }
            }),
        ),
        cell(
            "private-source",
            "selected",
            "e",
            json!({"view":{"type":"note"},"source_ref":{"entity_id":"private-note"}}),
        ),
        cell(
            "other-sticky",
            "other",
            "a",
            json!({
                "view": { "type": "note" }, "source_ref": { "entity_id": "other-note" }
            }),
        ),
    ]);
    ok(&mut ws, &alice(), json!(create));

    assert_eq!(
        ws.capture_approval_surface("bob", "selected").unwrap_err().code,
        Code::NotFound
    );
    assert_eq!(
        ws.capture_approval_surface("alice", "selected-content")
            .unwrap_err()
            .code,
        Code::NotFound
    );
    let snapshot = ws.capture_approval_surface("alice", "selected").unwrap();
    assert_eq!(snapshot.canvas.title, "Approved Canvas");
    assert_eq!(snapshot.epoch, ws.epoch);
    assert_eq!(snapshot.head_seq, ws.head_seq);
    assert_eq!(
        snapshot
            .canvas
            .blocks
            .iter()
            .map(|block| block.id.as_str())
            .collect::<Vec<_>>(),
        ["frame", "rich", "sticky"]
    );
    assert_eq!(snapshot.canvas.blocks[1].text, "Approved native rich text.");
    assert_eq!(snapshot.canvas.blocks[2].text, "Approved sticky note.");
    assert!(snapshot
        .omissions
        .iter()
        .any(|notice| notice.contains("otherwise unavailable")));
    let encoded = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(encoded["canvas"]["blocks"][0]["x"], 40.0);
    assert_eq!(encoded["canvas"]["blocks"][0]["y"], -20.0);
    assert_eq!(encoded["canvas"]["blocks"][0]["kind"], "frame");
    let body = encoded.to_string();
    for excluded in [
        "Other surface secret.",
        "Private note secret.",
        "Speaker note secret.",
        "Different Canvas",
    ] {
        assert!(!body.contains(excluded), "{excluded}");
    }

    env.advance(1000);
    ok(
        &mut ws,
        &alice(),
        json!([cell(
            "later",
            "selected",
            "f",
            json!({
                "view": { "type": "label" }, "title": "Added after approval."
            })
        )]),
    );
    let later = ws.capture_approval_surface("alice", "selected").unwrap();
    assert!(later.head_seq > snapshot.head_seq);
    assert_ne!(later.captured_at, snapshot.captured_at);
    assert!(later
        .canvas
        .blocks
        .iter()
        .any(|block| block.text == "Added after approval."));
    assert!(!serde_json::to_string(&snapshot)
        .unwrap()
        .contains("Added after approval."));
}

#[test]
fn approval_viewer_loads_native_replica_rows_and_bounds_the_attachment_to_the_selected_surface() {
    let env = env();
    let created = env.svc.create_workspace(&alice(), "Native Preview", None).unwrap();
    let id = created["workspace_id"].as_str().unwrap();
    let handle = env.svc.workspace(id).unwrap();
    let mut ws = handle.lock().unwrap();
    let selected = b"\x89PNG\r\n\x1a\nselectable picture";
    let other = b"\x89PNG\r\n\x1a\nunrelated picture";
    let selected_object = ws.stage_asset(&alice(), selected).unwrap()["object_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let other_object = ws.stage_asset(&alice(), other).unwrap()["object_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let asset = |id: &str, folder: &str, object_id: &str| {
        json!({
            "op": "entity.create", "entity_id": id, "type_id": "buckyos.asset-ref",
            "parent_id": folder, "order_key": "b",
            "payload": { "object_id": object_id, "file_name": "Picture.png" }
        })
    };
    let mut create = Vec::new();
    create.extend(capture_surface("selected", "selected-content", "a", "Approved Canvas"));
    create.extend(capture_surface("other", "other-content", "b", "Sibling Secret Canvas"));
    create.extend([
        json!({
            "op": "entity.create", "entity_id": "approved-text", "type_id": "buckyos.richtext",
            "parent_id": "selected-content", "order_key": "a",
            "payload": { "content": { "type": "doc", "content": [
                { "type": "paragraph", "attrs": { "block_id": "p1" },
                  "content": [{ "type": "text", "text": "Native approved paragraph." }] }
            ] } }
        }),
        asset("approved-image", "selected-content", &selected_object),
        asset("sibling-image", "other-content", &other_object),
        note(
            "private-note",
            "selected-content",
            "c",
            "Private annotation.",
            "personal",
        ),
        cell(
            "approved-frame",
            "selected",
            "a",
            json!({
                "view": { "type": "frame" }, "title": "Approved Frame",
                "presentation": { "notes": "Private presenter note.", "caption": "Approved caption" }
            }),
        ),
        cell(
            "show-text",
            "selected",
            "b",
            json!({
                "view": { "type": "richtext" }, "source_ref": { "entity_id": "approved-text" }
            }),
        ),
        cell(
            "show-image",
            "selected",
            "c",
            json!({
                "view": { "type": "asset" }, "source_ref": { "entity_id": "approved-image" }
            }),
        ),
        cell(
            "sibling-ref",
            "selected",
            "d",
            json!({
                "view": { "type": "asset" }, "source_ref": { "entity_id": "sibling-image" }
            }),
        ),
        cell(
            "personal-ref",
            "selected",
            "e",
            json!({
                "view": { "type": "note" }, "source_ref": { "entity_id": "private-note" }
            }),
        ),
        cell(
            "sibling-ref-row",
            "other",
            "a",
            json!({
                "view": { "type": "asset" }, "source_ref": { "entity_id": "sibling-image" }
            }),
        ),
    ]);
    ok(&mut ws, &alice(), json!(create));

    assert_eq!(
        ws.capture_approval_viewer("bob", "selected").unwrap_err().code,
        Code::NotFound
    );
    let preview = ws.capture_approval_viewer("alice", "selected").unwrap();
    assert_eq!(preview.mode, "free");
    assert_eq!(preview.title, "Approved Canvas");
    assert_eq!(preview.epoch, ws.epoch);
    assert_eq!(preview.head_seq, ws.head_seq);
    assert_eq!(preview.assets.len(), 1);
    assert!(preview.assets_complete);
    assert_eq!(preview.assets[0].object_id, selected_object);
    assert_eq!(preview.assets[0].name, "Picture.png");
    assert_eq!(preview.assets[0].media_type, "image/png");

    let native = aiworkspace_core::replica::load_rows(&preview.rows).unwrap();
    assert_eq!(native.edges["approved-frame"].placement.as_ref().unwrap()["x"], 40);
    assert_eq!(
        native.entities["approved-frame"].payload["presentation"]["caption"],
        "Approved caption"
    );
    assert_eq!(
        native.entities["show-image"].payload["source_ref"]["entity_id"],
        "approved-image"
    );
    assert_eq!(
        native.richtexts["approved-text"].meta.ast["content"][0]["content"][0]["text"],
        "Native approved paragraph."
    );
    let raw = preview.rows.to_string();
    for excluded in [
        "Sibling Secret Canvas",
        "sibling-image",
        "Private annotation.",
        "Private presenter note.",
        &other_object,
    ] {
        assert!(!raw.contains(excluded), "{excluded}");
    }

    let version = Some((preview.epoch.as_str(), preview.head_seq));
    let attachment = ws
        .find_approval_viewer_asset("alice", "selected", &selected_object, version)
        .unwrap();
    assert_eq!(attachment.read(&env.objects).unwrap(), selected);
    assert_eq!(
        ws.find_approval_viewer_asset("alice", "selected", &other_object, version)
            .unwrap_err()
            .code,
        Code::NotFound
    );
    assert_eq!(
        ws.find_approval_viewer_asset("bob", "selected", &selected_object, version)
            .unwrap_err()
            .code,
        Code::NotFound
    );
    assert_eq!(
        ws.find_approval_viewer_asset("alice", "other", &selected_object, version)
            .unwrap_err()
            .code,
        Code::NotFound
    );
    ok(
        &mut ws,
        &alice(),
        json!([cell(
            "later",
            "selected",
            "f",
            json!({
                "view": { "type": "label" }, "title": "Live update."
            })
        )]),
    );
    assert_eq!(
        ws.find_approval_viewer_asset("alice", "selected", &selected_object, version)
            .unwrap_err()
            .code,
        Code::RevisionConflict
    );
    assert!(ws
        .find_approval_viewer_asset("alice", "selected", &selected_object, None)
        .is_ok());

    let fixture_db = rusqlite::Connection::open(ws.dir.join("doc.sqlite")).unwrap();
    fixture_db
        .execute(
            "UPDATE assets SET size=?1 WHERE object_id=?2",
            rusqlite::params![33 * 1024 * 1024, selected_object],
        )
        .unwrap();
    let preview = ws.capture_approval_viewer("alice", "selected").unwrap();
    assert!(!preview.assets_complete);
    assert!(preview.assets.is_empty());
}

#[test]
fn approval_projects_only_selected_table_rows_in_new_order() {
    let env = env();
    let ws = env.svc.create_workspace(&alice(), "Table Approval", None).unwrap();
    let handle = env.svc.workspace(ws["workspace_id"].as_str().unwrap()).unwrap();
    let mut ws = handle.lock().unwrap();
    let mut create = Vec::new();
    create.extend(capture_surface("selected", "selected-content", "a", "Selected"));
    create.extend(capture_surface("other", "other-content", "b", "Other"));
    for (id, folder) in [("selected-table", "selected-content"), ("secret-table", "other-content")] {
        create.push(json!({
            "op": "entity.create", "entity_id": id, "type_id": "buckyos.table-source",
            "parent_id": folder, "order_key": "a", "payload": {
                "fields": [{ "field_id": "name", "name": "Name", "type": "text" }]
            }
        }));
    }
    create.push(cell("visible-grid", "selected", "a", json!({
        "view": { "type": "table" }, "source_ref": { "entity_id": "selected-table" },
        "manual_order": { "a-row": "a" }, "row_order": {
            "z-row": "0000000000000000!secret-anchor!a",
            "secret-record": "0000000000000000!secret-anchor!b"
        }
    })));
    create.extend([
        json!({ "op": "table.insert_records", "source_id": "selected-table", "records": [
            { "record_id": "a-row", "values": { "name": "Alpha" } },
            { "record_id": "z-row", "values": { "name": "Zulu" } }
        ] }),
        json!({ "op": "table.insert_records", "source_id": "secret-table", "records": [
            { "record_id": "secret-record", "values": { "name": "Unrelated record" } }
        ] }),
    ]);
    ok(&mut ws, &alice(), json!(create));
    let preview = ws.capture_approval_viewer("alice", "selected").unwrap();
    let contents = aiworkspace_core::replica::load_rows(&preview.rows).unwrap();
    let view = &contents.entities["visible-grid"].payload;
    assert!(!view.contains_key("row_order"));
    assert_eq!(view["manual_order"], json!({ "z-row": "0000h", "a-row": "0001h" }));
    assert_eq!(contents.records.len(), 2);
    let raw = preview.rows.to_string();
    for excluded in ["secret-table", "secret-record", "secret-anchor", "Unrelated record"] {
        assert!(!raw.contains(excluded), "{excluded}");
    }
}
