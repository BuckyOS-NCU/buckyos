mod common;

use aiworkspace_core::access::Cap;
use aiworkspace_store::classroom::ClassroomKind;
use aiworkspace_store::entries::split_entry_id;
use aiworkspace_store::worklog::Query;
use aiworkspace_store::{Caller, Workspace};
use common::*;
use serde_json::{json, Value};
use std::sync::{Arc, Barrier};

const SHARED: &str = "classroom-surface";
const SHARED_CONTENT: &str = "classroom-content";
const PRIVATE: &str = "teacher-surface";
const PRIVATE_CONTENT: &str = "teacher-content";

fn teacher() -> Caller {
    Caller::verified_account("teacher-one", true, true)
}
fn guest() -> Caller {
    Caller::verified_account("guest-one", true, false)
}
fn student() -> Caller {
    Caller::verified_account("student-one", true, false)
}
fn denied_ids() -> [Caller; 4] {
    [
        Caller::anonymous(),
        Caller::verified_account("wrong-user", true, false),
        Caller::account("guest-one", true),
        Caller::agent_of("did:bns:some-agent", "guest-one"),
    ]
}

fn commit(ws: &mut Workspace, caller: &Caller, key: &str, ops: Value) -> Value {
    ws.commit(
        &json!({"protocol_version":aiworkspace_core::model::PROTOCOL_VERSION,
        "workspace_id":ws.workspace_id,"epoch":ws.epoch,"idempotency_key":key,"operations":ops}),
        caller,
        &Default::default(),
    )
}

fn add_surface(ws: &mut Workspace, id: &str, folder: &str, sort: &str) {
    let result = commit(
        ws,
        &teacher(),
        id,
        json!([
            {"op":"entity.create","entity_id":folder,"type_id":"buckyos.container","parent_id":"canvas-content","order_key":sort,
             "payload":{"kind":"folder","system":"surface_content","surface_id":id,"title":id}},
            {"op":"entity.create","entity_id":id,"type_id":"buckyos.container","parent_id":"surfaces","order_key":sort,
             "payload":{"kind":"surface","layout":{"mode":"free"},"title":id,"content_folder_id":folder}}
        ]),
    );
    assert_eq!(result["status"], "accepted", "{result}");
}

fn fixture() -> (Env, String) {
    let e = env();
    let result = e
        .svc
        .create_workspace(&teacher(), "Private fixture", None)
        .unwrap();
    let id = result["workspace_id"].as_str().unwrap().to_owned();
    let h = e.svc.workspace(&id).unwrap();
    let mut ws = h.lock().unwrap();
    add_surface(&mut ws, SHARED, SHARED_CONTENT, "a");
    add_surface(&mut ws, PRIVATE, PRIVATE_CONTENT, "b");
    (e, id)
}

fn local(v: &Value) -> &str {
    split_entry_id(v["entry_id"].as_str().unwrap()).unwrap().1
}

fn frame(id: &str, parent: &str) -> Value {
    json!({"op":"entity.create","entity_id":id,"type_id":"buckyos.cell","parent_id":parent,"order_key":"a",
           "placement":{"x":0,"y":0,"w":320,"h":220},"payload":{"view":{"type":"frame"},"title":"Synthetic"}})
}

#[test]
fn classroom_exact_guest_is_single_use_even_after_revocation_and_reopen() {
    let (e, id) = fixture();
    let h = e.svc.workspace(&id).unwrap();
    let offer = {
        let mut ws = h.lock().unwrap();
        assert!(ws.info(&guest()).is_err());
        assert!(ws
            .classroom_issue(
                &Caller::verified_account("teacher-one", true, false),
                "guest-one",
                SHARED,
                ClassroomKind::Guest
            )
            .is_err());
        assert!(ws
            .classroom_issue(
                &Caller::verified_account("other-teacher", true, true),
                "guest-one",
                SHARED,
                ClassroomKind::Guest
            )
            .is_err());
        assert!(ws
            .classroom_issue(&teacher(), "teacher-one", SHARED, ClassroomKind::Guest)
            .is_err());
        assert!(ws
            .classroom_issue(&teacher(), "guest-one", "data", ClassroomKind::Guest)
            .is_err());
        let offer = ws
            .classroom_issue(&teacher(), "guest-one", SHARED, ClassroomKind::Guest)
            .unwrap();
        for c in denied_ids() {
            assert!(ws.classroom_redeem(&c, local(&offer)).is_err());
        }
        assert!(ws.join_entry(&guest(), local(&offer), |_| Ok(())).is_err());
        offer
    };
    let local = local(&offer).to_owned();
    let barrier = Arc::new(Barrier::new(3));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let (h, barrier, local) = (h.clone(), barrier.clone(), local.clone());
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            h.lock().unwrap().classroom_redeem(&guest(), &local).is_ok()
        }));
    }
    barrier.wait();
    assert_eq!(
        workers
            .into_iter()
            .map(|t| t.join().unwrap())
            .filter(|ok| *ok)
            .count(),
        1
    );
    {
        let mut ws = h.lock().unwrap();
        assert!(ws.require_on(&guest(), Some(SHARED), Cap::Read).is_ok());
        assert!(ws
            .require_on(&guest(), Some(SHARED_CONTENT), Cap::Read)
            .is_ok());
        assert!(ws.require_on(&guest(), Some(PRIVATE), Cap::Read).is_err());
        assert!(ws.require_on(&guest(), None, Cap::Read).is_err());
        assert!(ws.stage_asset(&guest(), b"synthetic attachment").is_err());
        assert_eq!(
            commit(
                &mut ws,
                &guest(),
                "guest-frame",
                json!([frame("not-allowed", SHARED)])
            )["status"],
            "rejected"
        );
        assert!(ws
            .info(&Caller::delegated(&guest().identity_json(), "guest-one"))
            .is_err());
        ws.classroom_revoke(&teacher(), &local).unwrap();
        assert!(ws.info(&guest()).is_err());
    }
    drop(h);
    e.svc.close(&id);
    let h = e.svc.workspace(&id).unwrap();
    assert!(h
        .lock()
        .unwrap()
        .classroom_redeem(&guest(), &local)
        .is_err());

    let backup_owner = Caller::verified_account("teacher-one", false, false);
    let bytes = {
        let mut ws = h.lock().unwrap();
        let info = ws
            .export(&backup_owner, "personal_backup", true, None)
            .unwrap();
        fetched(&ws.objects, &info)
    };
    drop(h);
    e.svc
        .import(&backup_owner, &bytes, "restore", true)
        .unwrap();
    let reopened = e.svc.workspace(&id).unwrap();
    let ws = reopened.lock().unwrap();
    assert!(ws.is_classroom_entry(&local).unwrap());
    assert!(ws.classroom_redeem(&guest(), &local).is_err());
    assert!(ws.info(&guest()).is_err());
}

#[test]
fn classroom_approval_requires_recipient_redeem_and_allows_a_new_request() {
    let (e, id) = fixture();
    let h = e.svc.workspace(&id).unwrap();
    let ws = h.lock().unwrap();
    assert!(ws.classroom_request(&Caller::anonymous(), SHARED).is_err());
    for _ in 0..2 {
        assert_eq!(ws.classroom_request(&guest(), SHARED).unwrap()["state"], "pending");
    }
    assert!(ws.info(&guest()).is_err());
    assert!(ws.classroom_requests(&guest()).is_err());
    let list = ws.classroom_requests(&teacher()).unwrap();
    let requests = list["requests"].as_array().unwrap();
    assert_eq!(requests.len(), 1);
    let request = requests[0]["request_id"].as_str().unwrap();
    assert!(ws.classroom_approve(&teacher(), request, "wrong-user").is_err());
    let grant = ws.classroom_approve(&teacher(), request, "guest-one").unwrap();
    assert!(ws.classroom_approve(&teacher(), request, "guest-one").is_err());
    assert!(ws.info(&guest()).is_err());
    assert_eq!(ws.classroom_requests(&teacher()).unwrap()["requests"][0]["state"], "ready");
    assert_eq!(ws.classroom_request(&guest(), SHARED).unwrap(),
        json!({"ok":true,"state":"ready","entry_id":grant["entry_id"]}));
    let observer = Caller::verified_account("observer", true, false);
    assert_eq!(ws.classroom_request(&observer, SHARED).unwrap()["state"], "pending");
    assert!(ws.classroom_redeem(&observer, local(&grant)).is_err());
    ws.classroom_redeem(&guest(), local(&grant)).unwrap();
    assert_eq!(ws.classroom_request(&guest(), SHARED).unwrap()["state"], "joined");
    assert!(ws.require_on(&guest(), Some(SHARED), Cap::Read).is_ok());
    assert!(ws.require_on(&guest(), Some(PRIVATE), Cap::Read).is_err());
    assert!(ws.classroom_redeem(&guest(), local(&grant)).is_err());
    ws.classroom_revoke(&teacher(), local(&grant)).unwrap();
    assert!(ws.info(&guest()).is_err());
    assert!(ws.classroom_redeem(&guest(), local(&grant)).is_err());
    assert_eq!(ws.classroom_request(&guest(), SHARED).unwrap()["state"], "pending");
    let list = ws.classroom_requests(&teacher()).unwrap();
    let requests = list["requests"].as_array().unwrap();
    assert!(requests.iter().any(|row| row["request_id"] == request && row["state"] == "revoked"));
    let pending_guest = |row: &&Value| row["applicant"] == "guest-one" && row["state"] == "pending";
    let fresh = requests.iter().find(pending_guest).unwrap()["request_id"].as_str().unwrap();
    let replacement = ws.classroom_approve(&teacher(), fresh, "guest-one").unwrap();
    assert_ne!(grant["entry_id"], replacement["entry_id"]);
    e.advance(86_400_001);
    assert!(ws.classroom_redeem(&guest(), local(&replacement)).is_err());
    assert_eq!(ws.classroom_request(&guest(), SHARED).unwrap()["state"], "pending");
    let history = ws.acl_audit(&teacher(), 0, 200).unwrap();
    let actions: Vec<_> = history["items"].as_array().unwrap().iter()
        .filter_map(|row| row["action"].as_str()).filter(|action| action.starts_with("classroom."))
        .collect();
    assert_eq!(actions, ["classroom.request", "classroom.approve", "classroom.request",
        "classroom.redeem", "classroom.revoke", "classroom.request", "classroom.approve", "classroom.request"]);
}

#[test]
fn classroom_scoped_student_activity() {
    let (e, id) = fixture();
    let h = e.svc.workspace(&id).unwrap();
    {
        let mut ws = h.lock().unwrap();
        let offer = ws
            .classroom_issue(&teacher(), "student-one", SHARED, ClassroomKind::Student)
            .unwrap();
        ws.classroom_redeem(&student(), local(&offer)).unwrap();
        assert!(ws.require_on(&student(), None, Cap::Structure).is_err());
        assert!(ws.require_on(&student(), Some(PRIVATE), Cap::Read).is_err());
        assert_eq!(
            commit(
                &mut ws,
                &student(),
                "frame",
                json!([frame("student-frame", SHARED)])
            )["status"],
            "accepted"
        );
        assert_eq!(
            commit(
                &mut ws,
                &student(),
                "off-scope",
                json!([frame("outside", PRIVATE)])
            )["status"],
            "rejected"
        );
        let data = ws
            .stage_asset(&student(), b"synthetic student attachment")
            .unwrap();
        let object = data["object_id"].as_str().unwrap();
        let reference = |id, parent| {
            json!({"op":"entity.create","entity_id":id,"type_id":"buckyos.asset-ref",
            "parent_id":parent,"order_key":"a","payload":{"object_id":object,"media_type":data["media_type"],"size":data["size"]}})
        };
        assert_eq!(
            commit(
                &mut ws,
                &student(),
                "attach",
                json!([reference("student-asset", SHARED_CONTENT)])
            )["status"],
            "accepted"
        );
        assert_eq!(
            commit(
                &mut ws,
                &student(),
                "attach-private",
                json!([reference("outside-asset", PRIVATE_CONTENT)])
            )["status"],
            "rejected"
        );
        assert!(ws.can_read_asset(&student(), object).unwrap());
        assert!(ws
            .can_read_asset(&Caller::verified_account("stranger", true, false), object)
            .is_err());
        assert!(ws.require_run(&student(), Some(SHARED)).is_err());
        ws.classroom_revoke(&teacher(), local(&offer)).unwrap();
        assert!(ws.can_read_asset(&student(), object).is_err());
    }
    let log = e
        .svc
        .work_log(&teacher(), &id, &Query::parse(&json!({})).unwrap(), vec![])
        .unwrap();
    let rows = log["entries"].as_array().unwrap();
    assert!(rows.iter().any(|r| r["action"] == "classroom.issue"));
    assert!(rows
        .iter()
        .any(|r| r["kind"] == "commit" && r["actor"] == "student-one"));
}
