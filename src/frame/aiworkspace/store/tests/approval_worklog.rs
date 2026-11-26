mod common;

use aiworkspace_core::Code;
use aiworkspace_store::approval_worklog::WorkLogQuery;
use aiworkspace_store::classroom::ClassroomKind;
use aiworkspace_store::entries::split_entry_id;
use aiworkspace_store::{Caller, Workspace};
use common::{env, ok};
use serde_json::{json, Value};

fn add_surface(ws: &mut Workspace, teacher: &Caller, id: &str, folder: &str, order: &str) {
    ok(
        ws,
        teacher,
        json!([
            {"op":"entity.create","entity_id":folder,"type_id":"buckyos.container","parent_id":"canvas-content","order_key":order,
             "payload":{"kind":"folder","system":"surface_content","surface_id":id,"title":id}},
            {"op":"entity.create","entity_id":id,"type_id":"buckyos.container","parent_id":"surfaces","order_key":order,
             "payload":{"kind":"surface","layout":{"mode":"free"},"title":id,"content_folder_id":folder}}
        ]),
    );
}

fn note(id: &str, folder: &str, order: &str, body: &str, target: Option<&str>, scope: &str) -> Value {
    let mut payload = json!({"kind":"note","body":body});
    if let Some(target) = target {
        payload["target"] = json!({"entity_id":target});
    }
    json!({"op":"entity.create","entity_id":id,"type_id":"buckyos.annotation","parent_id":folder,
           "order_key":order,"scope":scope,"payload":payload})
}

#[test]
fn scoped_approval_work_log_records_native_operations_and_survives_reopen_and_revocation() {
    let teacher = Caller::verified_account("teacher-one", true, true);
    let student = Caller::verified_account("student-one", true, false);
    let env = env();
    let id = env.svc.create_workspace(&teacher, "Course", None).unwrap()["workspace_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let handle = env.svc.workspace(&id).unwrap();
    let mut ws = handle.lock().unwrap();
    add_surface(&mut ws, &teacher, "selected", "selected-content", "a");
    add_surface(&mut ws, &teacher, "other", "other-content", "b");
    let offer = ws
        .classroom_issue(&teacher, "student-one", "selected", ClassroomKind::Student)
        .unwrap();
    let local = split_entry_id(offer["entry_id"].as_str().unwrap())
        .unwrap()
        .1
        .to_owned();
    ws.classroom_redeem(&student, &local).unwrap();
    assert_eq!(ws.info(&student).unwrap()["classroom_audited_download"], true);
    assert!(ws.info(&teacher).unwrap().get("classroom_audited_download").is_none());
    let roster = vec!["teacher-one".into(), "student-one".into()];
    let query = |principals, before, limit| WorkLogQuery {
        cutoff_ms: None,
        cutoff: None,
        before,
        limit,
        principals,
    };

    let file = b"synthetic accepted student attachment";
    let staged = ws.stage_asset(&student, file).unwrap();
    let object_id = staged["object_id"].as_str().unwrap();
    assert!(ws
        .approval_work_log("teacher-one", "selected", &query(&roster, None, 50))
        .unwrap()
        .events
        .is_empty());
    let ops = json!([
        {"op":"entity.create","entity_id":"student-frame","type_id":"buckyos.cell","parent_id":"selected","order_key":"a",
         "placement":{"x":0,"y":0,"w":240,"h":180},"payload":{"view":{"type":"frame"},"title":"Student Frame"}},
        {"op":"entity.create","entity_id":"student-file","type_id":"buckyos.asset-ref","parent_id":"selected-content","order_key":"a",
         "payload":{"object_id":object_id,"media_type":"text/plain","file_name":"Student.txt"}}
    ]);
    let mut request = common::request(&ws, ops);
    request["message"] = json!("Teacher replied");
    assert_eq!(ws.commit(&request, &student, &Default::default())["status"], "accepted");
    assert_eq!(ws.commit(&request, &student, &Default::default())["status"], "accepted");
    let page = ws
        .approval_work_log("teacher-one", "selected", &query(&roster, None, 50))
        .unwrap();
    assert_eq!(
        page.events.iter().map(|row| row.kind.as_str()).collect::<Vec<_>>(),
        ["asset_uploaded", "frame_created"]
    );

    let actual = ws.download_approval_asset(&student, "student-file").unwrap();
    assert_eq!(actual, file);
    assert!(ws.download_approval_asset(&student, "nonexistent").is_err());
    ok(
        &mut ws,
        &teacher,
        json!([
            note(
                "root-note",
                "selected-content",
                "b",
                "Original shared note",
                None,
                "shared"
            ),
            note(
                "private-response",
                "selected-content",
                "c",
                "Private teacher content",
                Some("root-note"),
                "personal"
            ),
            note("other-note", "other-content", "a", "Other Surface note", None, "shared"),
            note(
                "other-response",
                "other-content",
                "b",
                "Sibling teacher reply",
                Some("other-note"),
                "shared"
            ),
            note(
                "unrelated-response",
                "data",
                "z",
                "Out-of-scope teacher reply",
                Some("root-note"),
                "shared"
            ),
            note(
                "real-response",
                "selected-content",
                "d",
                "Genuine teacher reply",
                Some("root-note"),
                "shared"
            )
        ]),
    );
    let cut = ws.approval_work_log_cutoff("selected").unwrap();
    let first = ws
        .approval_work_log("teacher-one", "selected", &query(&roster, None, 2))
        .unwrap();
    assert_eq!(
        first.events.iter().map(|row| row.kind.as_str()).collect::<Vec<_>>(),
        ["teacher_replied", "asset_downloaded"]
    );
    assert_eq!(first.events[0].summary.as_deref(), Some("Genuine teacher reply"));
    assert_eq!(first.events[0].role, "teacher");
    assert_eq!(first.events[1].role, "student");
    let more = ws
        .approval_work_log("teacher-one", "selected", &query(&roster, first.next_before, 2))
        .unwrap();
    assert_eq!(
        more.events.iter().map(|row| row.kind.as_str()).collect::<Vec<_>>(),
        ["asset_uploaded", "frame_created"]
    );
    assert!(more.next_before.is_none());
    assert_eq!(
        ws.approval_work_log("student-one", "selected", &query(&roster, None, 50))
            .err()
            .unwrap()
            .code,
        Code::NotFound
    );
    ws.classroom_revoke(&teacher, &local).unwrap();
    assert!(ws.download_approval_asset(&student, "student-file").is_err());
    ok(
        &mut ws,
        &teacher,
        json!([note(
            "late-reply",
            "selected-content",
            "e",
            "After capture",
            Some("root-note"),
            "shared"
        )]),
    );
    drop(ws);
    env.svc.close(&id);
    let reopened = env.svc.workspace(&id).unwrap();
    let ws = reopened.lock().unwrap();
    let frozen = WorkLogQuery {
        cutoff: Some((&cut.epoch, cut.last_event_seq)),
        ..query(&roster, None, 50)
    };
    let page = ws.approval_work_log("teacher-one", "selected", &frozen).unwrap();
    assert_eq!(page.events.len(), 4);
    assert_eq!(page.events[0].summary.as_deref(), Some("Genuine teacher reply"));
    let teacher_only = vec!["teacher-one".into()];
    let page = ws
        .approval_work_log("teacher-one", "selected", &query(&teacher_only, None, 50))
        .unwrap();
    assert_eq!(page.events.len(), 2);
    assert!(page.events.iter().all(|event| event.role == "teacher"));

    let fixture_db = rusqlite::Connection::open(ws.dir.join("doc.sqlite")).unwrap();
    fixture_db
        .execute(
            "UPDATE entities SET scope='user:teacher-one' WHERE entity_id='student-file'",
            [],
        )
        .unwrap();
    let ordinary_owner = Caller::verified_account("teacher-one", false, false);
    assert_eq!(
        ws.download_approval_asset(&ordinary_owner, "student-file").unwrap(),
        file
    );
    assert_eq!(
        ws.approval_work_log("teacher-one", "selected", &query(&roster, None, 50))
            .unwrap()
            .events
            .len(),
        5
    );
}
