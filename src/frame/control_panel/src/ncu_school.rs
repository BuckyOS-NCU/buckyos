use crate::RpcAuthPrincipal;
use buckyos_api::{
    get_buckyos_api_runtime, get_rdb_instance_in, AuthMethod, RdbBackend, RdbPartition,
    SystemConfigError, TokenPrincipalKind, CONTROL_PANEL_SERVICE_NAME,
};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

const INSTANCE: &str = "ncu-school";
const INSTANCE_VERSION: u64 = 1;
const SPEC: &str = "services/control-panel/spec";
const SCHEMA: &str = include_str!("ncu_school_schema.sql");
const FIELDS: &[&str] = &[
    "userId",
    "realName",
    "realNameEn",
    "gender",
    "userType",
    "userTypeLabel",
    "roleId",
    "collegeCode",
    "collegeName",
    "collegeNameEn",
    "majorCode",
    "majorName",
    "majorNameEn",
    "className",
    "grade",
    "eduLength",
    "degreeLevel",
    "currentStatus",
    "title",
];
const IDENTITY_FIELDS: &[&str] = &[
    "identityKey",
    "userId",
    "realName",
    "roleId",
    "terminal",
    "roleName",
    "studentSubtype",
    "degreeLevel",
    "deptId",
    "deptName",
    "subLabel",
    "current",
];

#[derive(Clone, Copy, Debug)]
pub(super) struct Unavailable;

fn admitted_fields(source: &Map<String, Value>, fields: &[&str]) -> Map<String, Value> {
    fields
        .iter()
        .filter_map(|&key| {
            let value = source.get(key)?;
            (!value.is_array() && !value.is_object()).then(|| (key.to_owned(), value.clone()))
        })
        .collect()
}

pub(super) fn verified_dossier(data: &Value) -> Value {
    let Some(source) = data.as_object() else {
        return Value::Null;
    };
    let mut result = admitted_fields(source, FIELDS);
    match source.get("identities") {
        Some(Value::Array(rows)) => {
            let clean = rows
                .iter()
                .filter_map(Value::as_object)
                .map(|row| Value::Object(admitted_fields(row, IDENTITY_FIELDS)))
                .collect();
            result.insert("identities".into(), Value::Array(clean));
        }
        Some(Value::Null) => {
            result.insert("identities".into(), Value::Null);
        }
        _ => {}
    }
    Value::Object(result)
}

fn self_school_profile(principal: &RpcAuthPrincipal, target: &str) -> bool {
    principal.is_control_panel_session
        && principal.is_user_session
        && principal.principal_kind == TokenPrincipalKind::User
        && principal.username == target
        && principal.owner_user_id == target
        && !principal.session.is_external
        && !principal.session.is_sudo
        && matches!(
            principal.session.auth_method,
            Some(AuthMethod::NcuCas | AuthMethod::NcuPblTeacher)
        )
}

fn projection(dossier: &Value) -> Option<Value> {
    let text = |key: &str| {
        dossier
            .get(key)?
            .as_str()
            .filter(|value| !value.trim().is_empty() && !value.chars().any(char::is_control))
    };
    let user_type = text("userType")?;
    let (role, number_type) = match user_type {
        "TEACHER" => ("teacher", "staff"),
        "UNDERGRADUATE" | "GRADUATE" => ("student", "student"),
        _ => return None,
    };
    let mut result = serde_json::json!({
        "role": role,
        "school_number": text("userId")?,
        "school_number_type": number_type,
    });
    for (source, target) in [
        ("realName", "real_name"),
        ("userTypeLabel", "user_type_label"),
        ("collegeName", "college_name"),
        ("majorName", "major_name"),
        ("className", "class_name"),
        ("grade", "grade"),
        ("degreeLevel", "degree_level"),
        ("currentStatus", "current_status"),
        ("title", "title"),
    ] {
        if let Some(value) = text(source) {
            result[target] = Value::String(value.to_owned());
        }
    }
    Some(result)
}

fn registered(spec: &Value) -> Result<bool, Unavailable> {
    match spec.pointer("/spec_config/rdb_instances") {
        None => Ok(false),
        Some(Value::Object(instances)) => Ok(instances.contains_key(INSTANCE)),
        Some(_) => Err(Unavailable),
    }
}

async fn school_path() -> Result<Option<PathBuf>, Unavailable> {
    let runtime = get_buckyos_api_runtime().map_err(|_| Unavailable)?;
    let config = runtime
        .get_system_config_client()
        .await
        .map_err(|_| Unavailable)?;
    let spec = match config.get(SPEC).await {
        Ok(value) => serde_json::from_str(&value.value).map_err(|_| Unavailable)?,
        Err(SystemConfigError::KeyNotFound(_)) => return Ok(None),
        Err(_) => return Err(Unavailable),
    };
    if !registered(&spec)? {
        return Ok(None);
    }

    let instance = get_rdb_instance_in(
        CONTROL_PANEL_SERVICE_NAME,
        None,
        INSTANCE,
        RdbPartition::UserData,
    )
    .await
    .map_err(|_| Unavailable)?;
    if instance.backend != RdbBackend::Sqlite || instance.version != INSTANCE_VERSION {
        return Err(Unavailable);
    }
    let path = PathBuf::from(
        instance
            .connection
            .strip_prefix("sqlite:")
            .ok_or(Unavailable)?,
    );
    if !path.is_absolute() || !path.ends_with("ncu-school/school.sqlite") {
        return Err(Unavailable);
    }
    Ok(Some(path))
}

#[cfg(unix)]
fn open_store(path: &Path, write: bool) -> Result<Option<Connection>, Unavailable> {
    use std::fs::{self, OpenOptions, Permissions};
    use std::io::ErrorKind;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    let parent = path.parent().ok_or(Unavailable)?;
    let dir = fs::symlink_metadata(parent).map_err(|_| Unavailable)?;
    if !dir.is_dir() || dir.uid() != 0 || dir.mode() & 0o022 != 0 {
        return Err(Unavailable);
    }
    if dir.mode() & 0o777 != 0o700 {
        if !write {
            return match fs::symlink_metadata(path) {
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
                _ => Err(Unavailable),
            };
        }
        fs::set_permissions(parent, Permissions::from_mode(0o700)).map_err(|_| Unavailable)?;
    }
    if write {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
        {
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(_) => return Err(Unavailable),
        }
    }
    let file = match fs::symlink_metadata(path) {
        Ok(file) => file,
        Err(error) if !write && error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(Unavailable),
    };
    if !file.is_file() || file.uid() != 0 || file.mode() & 0o777 != 0o600 || file.nlink() != 1 {
        return Err(Unavailable);
    }
    let access = if write {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    };
    let connection = Connection::open_with_flags(path, access | OpenFlags::SQLITE_OPEN_NOFOLLOW)
        .map_err(|_| Unavailable)?;
    connection
        .busy_timeout(Duration::from_secs(3))
        .map_err(|_| Unavailable)?;
    Ok(Some(connection))
}

#[cfg(not(unix))]
fn open_store(_path: &Path, _write: bool) -> Result<Option<Connection>, Unavailable> {
    Err(Unavailable)
}

fn schema_version(db: &Connection) -> Result<u64, Unavailable> {
    db.pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|_| Unavailable)
}

fn write_dossier(
    db: &mut Connection,
    account_id: &str,
    dossier: Value,
    verified_at: i64,
) -> Result<(), Unavailable> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| Unavailable)?;
    match schema_version(&tx)? {
        0 => {
            tx.execute_batch(SCHEMA).map_err(|_| Unavailable)?;
            tx.pragma_update(None, "user_version", INSTANCE_VERSION)
                .map_err(|_| Unavailable)?;
        }
        INSTANCE_VERSION => {}
        _ => return Err(Unavailable),
    }
    tx.execute(
        "INSERT INTO ncu_school_dossiers (account_id, dossier_json, verified_at_ms) VALUES (?1, ?2, ?3)
         ON CONFLICT(account_id) DO UPDATE SET dossier_json=excluded.dossier_json,
             verified_at_ms=excluded.verified_at_ms
         WHERE excluded.verified_at_ms >= ncu_school_dossiers.verified_at_ms",
        params![account_id, dossier.to_string(), verified_at],
    ).map_err(|_| Unavailable)?;
    tx.commit().map_err(|_| Unavailable)
}

pub(super) async fn save_verified(
    account_id: &str,
    dossier: Value,
    verified_at: i64,
) -> Result<(), Unavailable> {
    let Some(path) = school_path().await? else {
        return Ok(());
    };
    let account_id = account_id.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut db = open_store(&path, true)?.ok_or(Unavailable)?;
        write_dossier(&mut db, &account_id, dossier, verified_at)
    })
    .await
    .map_err(|_| Unavailable)?
}

pub(super) async fn own_projection(principal: &RpcAuthPrincipal, target: &str) -> Option<Value> {
    if !self_school_profile(principal, target) {
        return None;
    }
    let read = read_own_dossier(target).await;
    if read.is_err() {
        log::warn!("NCU academic profile read failed");
    }
    read.ok().flatten()
}

async fn read_own_dossier(account_id: &str) -> Result<Option<Value>, Unavailable> {
    let Some(path) = school_path().await? else {
        return Ok(None);
    };
    let account_id = account_id.to_owned();
    tokio::task::spawn_blocking(move || {
        let Some(db) = open_store(&path, false)? else {
            return Ok(None);
        };
        if schema_version(&db)? != INSTANCE_VERSION {
            return Err(Unavailable);
        }
        let raw: Option<String> = db
            .query_row(
                "SELECT dossier_json FROM ncu_school_dossiers WHERE account_id=?1",
                [&account_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| Unavailable)?;
        raw.map(|data| {
            serde_json::from_str(&data)
                .map_err(|_| Unavailable)
                .map(|document| projection(&document))
        })
        .transpose()
        .map(Option::flatten)
    })
    .await
    .map_err(|_| Unavailable)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn school_dossier_whitelists_documented_data_at_each_level() {
        let mut provider = json!({
            "userId":"000042","userType":"TEACHER","collegeCode":"021","realName":"Test",
            "realNameEn":"TEST","gender":"fixture","userTypeLabel":"Test teacher","roleId":"fixture",
            "collegeName":"School","collegeNameEn":"School","majorCode":"","majorName":null,"majorNameEn":"",
            "className":"","grade":"2024","eduLength":"4","degreeLevel":"fixture","currentStatus":"active","title":"Test",
            "identities":[{"identityKey":"fixture","userId":"000042","realName":"Test","roleId":"fixture",
                "terminal":"TEACHER","roleName":"Teacher","studentSubtype":"","degreeLevel":"fixture",
                "deptId":"021","deptName":"School","subLabel":"fixture","current":true}]
        });
        let original = provider.clone();
        provider["authTicket"] = json!("fixture-ticket");
        provider["expiresIn"] = json!(7200);
        provider["code"] = json!("fixture-code");
        provider["identities"][0]["authTicket"] = json!("nested-fixture-ticket");
        provider["opaque"] = json!({"secret": "fixture"});
        let stored = verified_dossier(&provider);
        assert_eq!(stored, original);
        let shown = projection(&stored).unwrap();
        assert_eq!(shown["school_number"], "000042");
        assert_eq!(shown["school_number_type"], "staff");
        assert!(shown.get("identities").is_none());
        assert!(shown.get("collegeCode").is_none());
    }

    #[test]
    fn school_profile_is_exact_account_and_campus_session_only() {
        let mut session = RpcAuthPrincipal {
            username: "fixture".into(),
            owner_user_id: "fixture".into(),
            authenticated_app_id: CONTROL_PANEL_SERVICE_NAME.into(),
            user_type: crate::UserType::Limited,
            owner_did: String::new(),
            is_user_session: true,
            is_control_panel_session: true,
            principal_kind: TokenPrincipalKind::User,
            agent_permission_mode: None,
            session: crate::PrincipalSession {
                auth_method: Some(AuthMethod::NcuCas),
                ..Default::default()
            },
        };
        assert!(self_school_profile(&session, "fixture"));
        assert!(!self_school_profile(&session, "other"));
        session.session.auth_method = Some(AuthMethod::NcuPblTeacher);
        assert!(self_school_profile(&session, "fixture"));
        session.session.is_sudo = true;
        assert!(!self_school_profile(&session, "fixture"));
        session.session.is_sudo = false;
        session.is_control_panel_session = false;
        assert!(!self_school_profile(&session, "fixture"));
        session.is_control_panel_session = true;
        session.principal_kind = TokenPrincipalKind::App;
        assert!(!self_school_profile(&session, "fixture"));
        session.principal_kind = TokenPrincipalKind::User;
        session.session.auth_method = Some(AuthMethod::Password);
        assert!(!self_school_profile(&session, "fixture"));
    }

    #[test]
    fn school_store_uses_latest_snapshot_and_preserves_legacy_registration() {
        assert!(!registered(&json!({"spec_config": {}})).unwrap());
        assert!(
            registered(&json!({"spec_config": {"rdb_instances": {"ncu-school": {}}}})).unwrap()
        );
        assert!(registered(&json!({"spec_config": {"rdb_instances": []}})).is_err());
        let mut db = Connection::open_in_memory().unwrap();
        write_dossier(
            &mut db,
            "fixture",
            json!({"userId":"0042","userType":"GRADUATE"}),
            2,
        )
        .unwrap();
        write_dossier(&mut db, "fixture", json!({"userId":"out-of-order"}), 1).unwrap();
        let data: String = db
            .query_row(
                "SELECT dossier_json FROM ncu_school_dossiers WHERE account_id=?",
                ["fixture"],
                |row| row.get(0),
            )
            .unwrap();
        let shown = projection(&serde_json::from_str(&data).unwrap()).unwrap();
        assert_eq!(shown["school_number"], "0042");
        assert_eq!(shown["school_number_type"], "student");
        assert_eq!(schema_version(&db).unwrap(), INSTANCE_VERSION);
    }
}
