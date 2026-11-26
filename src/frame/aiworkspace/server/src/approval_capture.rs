use crate::{blocking, pages, AppState};
use aiworkspace_core::id::{is_valid_id, is_workspace_id};
use aiworkspace_core::Code;
use aiworkspace_store::source::RESERVED_WORKSPACE_SLUGS;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use buckyos_api::{
    constant_time_eq, get_buckyos_api_runtime, is_ncu_handoff_token, ncu_cas_identity_digest, NcuCasConfig,
    NcuCasLoginRequest, UserSettings, UserType, NCU_CAS_CONFIG_FILE, NCU_CAS_READY_MARKER,
};
use bytes::Bytes;
use name_lib::OwnerDocument;
use percent_encoding::{percent_decode_str, utf8_percent_encode, NON_ALPHANUMERIC};
use serde::Deserialize;
use serde_json::json;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use url::Url;

pub const MAX_REQUEST_BYTES: usize = 4096;
pub const MAX_WORK_LOG_REQUEST_BYTES: usize = 24 * 1024;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_VIEWER_BYTES: usize = 8 * 1024 * 1024;
const KEY_FILE: &str = "ncu-pbl-approval.key";
const ZONE_HOST: &str = "buckyos.ncu.bin-go.cc";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureRequest {
    project_id: String,
    school_id: String,
    workspace_url: String,
}

struct Source {
    url: Url,
    workspace_id: String,
    surface_id: String,
}

impl Source {
    fn parse(input: &str) -> Option<Self> {
        if input.len() > 1024
            || !input.is_ascii()
            || input
                .bytes()
                .any(|b| b.is_ascii_whitespace() || b.is_ascii_control() || b == b'\\')
        {
            return None;
        }
        let rest = input.strip_prefix("https://")?;
        let (host, path_and_query) = rest.split_once('/')?;
        let (raw_path, raw_query) = path_and_query.split_once('?')?;
        let suffix = host.strip_prefix("aiws-")?.strip_suffix(ZONE_HOST)?.strip_suffix('.')?;
        if suffix.len() != 8 || !suffix.bytes().take(4).all(|b| b.is_ascii_digit()) {
            return None;
        }
        let workspace_id = format!("ws_{suffix}");
        if !is_workspace_id(&workspace_id) {
            return None;
        }
        let url = Url::parse(input).ok()?;
        if url.host_str() != Some(host) || url.fragment().is_some() {
            return None;
        }
        let mut slugs = raw_path.split('/');
        let workspace_slug = slugs.next()?;
        let surface_slug = slugs.next()?;
        if RESERVED_WORKSPACE_SLUGS.contains(&workspace_slug)
            || !valid_slug(workspace_slug)
            || !valid_slug(surface_slug)
            || slugs.next().is_some()
            || url.path().strip_prefix('/') != Some(raw_path)
        {
            return None;
        }
        let encoded_id = raw_query.strip_prefix("id=")?;
        let surface_id = percent_decode_str(encoded_id).decode_utf8().ok()?;
        if !is_valid_id(&surface_id) || (encoded_id != surface_id && encoded_id != encode_surface_id(&surface_id)) {
            return None;
        }
        let surface_id = surface_id.into_owned();
        Some(Self {
            url,
            workspace_id,
            surface_id,
        })
    }
}

fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 64
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn encode_surface_id(id: &str) -> String {
    utf8_percent_encode(id, NON_ALPHANUMERIC).to_string()
}

#[cfg(unix)]
fn load_approval_key(path: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;

    if !path.symlink_metadata().ok()?.is_file() {
        return None;
    }
    let file = File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.uid() != 0 || meta.gid() != 0 || meta.mode() & 0o777 != 0o600 || meta.len() > 45 {
        return None;
    }
    let mut key = String::new();
    file.take(46).read_to_string(&mut key).ok()?;
    let key = key.trim_end_matches(['\r', '\n']);
    is_ncu_handoff_token(key).then(|| key.to_string())
}

#[cfg(not(unix))]
fn load_approval_key(_path: &Path) -> Option<String> {
    None
}

fn authorized(headers: &HeaderMap, key: &str) -> bool {
    let mut supplied = headers.get_all(header::AUTHORIZATION).iter();
    let token = supplied
        .next()
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|v| is_ncu_handoff_token(v));
    supplied.next().is_none() && token.is_some_and(|token| constant_time_eq(token.as_bytes(), key.as_bytes()))
}

fn is_bound_account(binding: NcuCasLoginRequest, settings: &UserSettings, document: &OwnerDocument) -> bool {
    let binding = NcuCasLoginRequest {
        user_type: settings.user_type,
        ..binding
    };
    binding.matches_account(settings)
        && document
            .extra_info
            .get(NCU_CAS_READY_MARKER)
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        && binding.account_key(document).is_some()
}

fn json_body(status: StatusCode, value: Vec<u8>) -> Response {
    (
        status,
        [
            (header::CACHE_CONTROL, "private, no-store"),
            (header::CONTENT_TYPE, "application/json"),
        ],
        value,
    )
        .into_response()
}

fn reject(status: StatusCode, code: &'static str) -> Response {
    json_body(status, json!({ "error": code }).to_string().into_bytes())
}

async fn resolve_existing_owner(binding: NcuCasLoginRequest) -> Result<String, StatusCode> {
    let id = binding.user_id().ok_or(StatusCode::NOT_FOUND)?;
    let runtime = get_buckyos_api_runtime().map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let client = runtime
        .get_system_config_client()
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let settings_key = format!("users/{id}/settings");
    let document_key = format!("users/{id}/doc");
    let (settings, document) =
        tokio::try_join!(client.get(&settings_key), client.get(&document_key)).map_err(|error| match error {
            buckyos_api::SystemConfigError::KeyNotFound(_) => StatusCode::NOT_FOUND,
            _ => StatusCode::SERVICE_UNAVAILABLE,
        })?;
    let settings: UserSettings = serde_json::from_str(&settings.value).map_err(|_| StatusCode::NOT_FOUND)?;
    let document: OwnerDocument = serde_json::from_str(&document.value).map_err(|_| StatusCode::NOT_FOUND)?;
    if !is_bound_account(binding, &settings, &document) {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(id)
}

fn authorize(headers: &HeaderMap) -> Result<(PathBuf, String), Response> {
    let etc = buckyos_kit::get_buckyos_root_dir().join("etc");
    let key =
        load_approval_key(&etc.join(KEY_FILE)).ok_or_else(|| reject(StatusCode::NOT_FOUND, "capture_unavailable"))?;
    if !authorized(headers, &key) {
        return Err(reject(StatusCode::UNAUTHORIZED, "capture_unauthorized"));
    }
    Ok((etc, key))
}

fn invalid_request() -> Response {
    reject(StatusCode::BAD_REQUEST, "invalid_capture_request")
}

async fn verified_source(
    etc: &Path,
    project_id: &str,
    school_id: &str,
    workspace_url: &str,
) -> Result<(Source, String), Response> {
    aiworkspace_core::id::check_idempotency_key(project_id).map_err(|_| invalid_request())?;
    let source =
        Source::parse(workspace_url).ok_or_else(|| reject(StatusCode::BAD_REQUEST, "invalid_workspace_url"))?;
    let config = config_at(etc).ok_or_else(|| reject(StatusCode::SERVICE_UNAVAILABLE, "capture_unavailable"))?;
    let digest =
        ncu_cas_identity_digest(&config.app_key, &config.identity_key, school_id).ok_or_else(invalid_request)?;
    let binding = NcuCasLoginRequest {
        identity_digest: digest,
        user_type: UserType::User,
    };
    let owner = resolve_existing_owner(binding).await.map_err(|status| {
        if status == StatusCode::NOT_FOUND {
            reject(StatusCode::NOT_FOUND, "canvas_not_found")
        } else {
            reject(StatusCode::SERVICE_UNAVAILABLE, "capture_unavailable")
        }
    })?;
    Ok((source, owner))
}

fn canonical_url(url: &mut Url, workspace_slug: &str, title: &str, surface_id: &str) {
    url.set_path(&format!("/{workspace_slug}/{}", pages::surface_slug(title)));
    url.set_query(Some(&format!("id={}", encode_surface_id(surface_id))));
}

fn capture_error(error: aiworkspace_core::WsError) -> Response {
    match error.code {
        Code::NotFound | Code::PermissionDenied => reject(StatusCode::NOT_FOUND, "canvas_not_found"),
        Code::LimitExceeded => reject(StatusCode::PAYLOAD_TOO_LARGE, "capture_too_large"),
        Code::RevisionConflict => reject(StatusCode::CONFLICT, "canvas_version_changed"),
        _ => reject(StatusCode::SERVICE_UNAVAILABLE, "capture_unavailable"),
    }
}

fn capture_json(result: aiworkspace_core::WsResult<serde_json::Value>, limit: usize) -> Response {
    match result {
        Ok(value) => {
            let bytes = value.to_string().into_bytes();
            if bytes.len() > limit {
                return reject(StatusCode::PAYLOAD_TOO_LARGE, "capture_too_large");
            }
            json_body(StatusCode::OK, bytes)
        }
        Err(error) => capture_error(error),
    }
}

pub async fn capture(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    let etc = match authorize(&headers) {
        Ok((etc, _key)) => etc,
        Err(response) => return response,
    };
    let Ok(input) = serde_json::from_slice::<CaptureRequest>(&body) else {
        return invalid_request();
    };
    let (
        Source {
            mut url,
            workspace_id,
            surface_id,
        },
        owner,
    ) = match verified_source(&etc, &input.project_id, &input.school_id, &input.workspace_url).await {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let result = blocking(move || {
        state.with_ws(&workspace_id, |ws| {
            let captured = ws.capture_approval_surface(&owner, &surface_id)?;
            canonical_url(&mut url, &ws.workspace_slug(), &captured.canvas.title, &surface_id);
            Ok(json!({
                "schema_version": 1, "project_id": input.project_id,
                "workspace_url": url.as_str(), "workspace_id": workspace_id,
                "surface_id": surface_id, "epoch": captured.epoch,
                "head_seq": captured.head_seq, "captured_at": captured.captured_at,
                "canvas": captured.canvas, "omissions": captured.omissions,
            }))
        })
    })
    .await;
    capture_json(result, MAX_RESPONSE_BYTES)
}

pub async fn viewer(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    let etc = match authorize(&headers) {
        Ok((etc, _key)) => etc,
        Err(response) => return response,
    };
    let Ok(input) = serde_json::from_slice::<CaptureRequest>(&body) else {
        return invalid_request();
    };
    let (
        Source {
            mut url,
            workspace_id,
            surface_id,
        },
        owner,
    ) = match verified_source(&etc, &input.project_id, &input.school_id, &input.workspace_url).await {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let result = blocking(move || {
        state.with_ws(&workspace_id, |ws| {
            let captured = ws.capture_approval_viewer(&owner, &surface_id)?;
            canonical_url(&mut url, &ws.workspace_slug(), &captured.title, &surface_id);
            Ok(viewer_response(
                &input.project_id,
                &url,
                &workspace_id,
                &surface_id,
                captured,
            ))
        })
    })
    .await;
    capture_json(result, MAX_VIEWER_BYTES)
}

fn viewer_response(
    project_id: &str,
    url: &Url,
    workspace_id: &str,
    surface_id: &str,
    captured: aiworkspace_store::approval_viewer::ApprovalViewer,
) -> serde_json::Value {
    json!({
                "schema_version": 2, "project_id": project_id,
                "workspace_url": url.as_str(), "workspace_id": workspace_id,
                "surface_id": surface_id, "epoch": captured.epoch,
                "head_seq": captured.head_seq, "captured_at": captured.captured_at,
                "title": captured.title, "mode": captured.mode,
                "principal": "pbl-approval-reviewer",
                "rows": captured.rows, "assets": captured.assets, "omissions": captured.omissions,
                "work_log_cutoff": captured.work_log_cutoff, "assets_complete": captured.assets_complete,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewerAssetRequest {
    project_id: String,
    school_id: String,
    workspace_url: String,
    object_id: String,
    epoch: Option<String>,
    head_seq: Option<u64>,
}

impl ViewerAssetRequest {
    fn valid(&self) -> bool {
        if !aiworkspace_core::canonical::is_obj_id(&self.object_id) {
            return false;
        }
        match (self.epoch.as_deref(), self.head_seq) {
            (None, None) => true,
            (Some(epoch), Some(_)) => !epoch.is_empty() && epoch.len() <= 128,
            _ => false,
        }
    }
}

pub async fn viewer_asset(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    let etc = match authorize(&headers) {
        Ok((etc, _key)) => etc,
        Err(response) => return response,
    };
    let Ok(input) = serde_json::from_slice::<ViewerAssetRequest>(&body) else {
        return invalid_request();
    };
    if !input.valid() {
        return invalid_request();
    }
    let (source, owner) = match verified_source(&etc, &input.project_id, &input.school_id, &input.workspace_url).await {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let result = blocking(move || {
        let version = input.epoch.as_deref().zip(input.head_seq);
        let (asset, objects) = state.with_ws(&source.workspace_id, |ws| {
            let asset = ws.find_approval_viewer_asset(&owner, &source.surface_id, &input.object_id, version)?;
            Ok((asset, ws.objects.clone()))
        })?;
        let bytes = asset.read(&objects)?;
        Ok((asset.media_type, bytes))
    })
    .await;
    match result {
        Ok((media, bytes)) => {
            let disposition = if media == "application/octet-stream" {
                "attachment"
            } else {
                "inline"
            };
            (
                [
                    (header::CACHE_CONTROL, "private, no-store".to_owned()),
                    (header::CONTENT_TYPE, media),
                    (header::CONTENT_DISPOSITION, disposition.to_owned()),
                    (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
                    (
                        header::CONTENT_SECURITY_POLICY,
                        "sandbox; default-src 'none'".to_owned(),
                    ),
                ],
                bytes,
            )
                .into_response()
        }
        Err(error) => capture_error(error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkLogRequest {
    project_id: String,
    version: u64,
    school_id: String,
    workspace_url: String,
    before: Option<String>,
    limit: usize,
    cutoff_at: Option<String>,
    cutoff_epoch: Option<String>,
    cutoff_event_seq: Option<u64>,
    #[serde(default)]
    participant_school_ids: Vec<String>,
}

fn work_log_cursor(value: &str) -> Option<u64> {
    let digits = value.strip_prefix("e_")?;
    if digits.is_empty() || digits.len() > 19 || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits
        .parse::<u64>()
        .ok()
        .filter(|seq| *seq > 0 && *seq <= i64::MAX as u64)
}

fn config_at(etc: &Path) -> Option<NcuCasConfig> {
    File::open(etc.join(NCU_CAS_CONFIG_FILE))
        .ok()
        .and_then(NcuCasConfig::read)
        .filter(|config| config.callback_url.host_str() == Some(ZONE_HOST))
}

fn project_actors(
    input: &WorkLogRequest,
    config: &NcuCasConfig,
    approval_key: &str,
) -> Option<std::collections::BTreeMap<String, String>> {
    let key_hex: String = approval_key.bytes().map(|value| format!("{value:02x}")).collect();
    let domain = format!("pbl-approval-work-log:actor:v1:{}", input.project_id);
    std::iter::once(&input.school_id)
        .chain(input.participant_school_ids.iter())
        .map(|school_id| {
            let digest = ncu_cas_identity_digest(&config.app_key, &config.identity_key, school_id)?;
            let principal = NcuCasLoginRequest {
                identity_digest: digest,
                user_type: UserType::Limited,
            }
            .user_id()?;
            let alias = format!("a_{}", ncu_cas_identity_digest(&domain, &key_hex, school_id)?);
            Some((principal, alias))
        })
        .collect()
}

pub async fn work_log(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    use aiworkspace_store::approval_worklog::{WorkLogQuery, MAX_PAGE_SIZE};

    let (etc, key) = match authorize(&headers) {
        Ok(context) => context,
        Err(response) => return response,
    };
    let Ok(input) = serde_json::from_slice::<WorkLogRequest>(&body) else {
        return invalid_request();
    };
    if input.version == 0
        || !(1..=MAX_PAGE_SIZE).contains(&input.limit)
        || input.participant_school_ids.len() > 128
        || input
            .participant_school_ids
            .iter()
            .any(|id| id.is_empty() || id.len() > 128)
        || input.cutoff_event_seq.is_some_and(|seq| seq > i64::MAX as u64)
        || input
            .cutoff_epoch
            .as_ref()
            .is_some_and(|epoch| epoch.is_empty() || epoch.len() > 128)
        || input.cutoff_epoch.is_some() != input.cutoff_event_seq.is_some()
        || (input.cutoff_epoch.is_some() && input.cutoff_at.is_none())
    {
        return invalid_request();
    }
    let before = input.before.as_deref().and_then(work_log_cursor);
    if input.before.is_some() && before.is_none() {
        return invalid_request();
    }
    let cutoff_ms = input
        .cutoff_at
        .as_deref()
        .filter(|stamp| stamp.len() <= 64)
        .and_then(aiworkspace_core::value::parse_datetime_ms);
    if input.cutoff_at.is_some() && cutoff_ms.is_none() {
        return invalid_request();
    }
    let (source, owner) = match verified_source(&etc, &input.project_id, &input.school_id, &input.workspace_url).await {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let Some(config) = config_at(&etc) else {
        return reject(StatusCode::SERVICE_UNAVAILABLE, "capture_unavailable");
    };
    let Some(actors) = project_actors(&input, &config, &key) else {
        return invalid_request();
    };
    let principals: Vec<String> = actors.keys().cloned().collect();
    let result = blocking(move || {
        let query = WorkLogQuery {
            cutoff_ms,
            cutoff: input.cutoff_epoch.as_deref().zip(input.cutoff_event_seq),
            before,
            limit: input.limit,
            principals: &principals,
        };
        let page = state.with_ws(&source.workspace_id, |ws| {
            ws.approval_work_log(&owner, &source.surface_id, &query)
        })?;
        Ok(work_log_response(&input, &source, page, &actors))
    })
    .await;
    capture_json(result, 256 * 1024)
}

fn work_log_response(
    input: &WorkLogRequest,
    source: &Source,
    page: aiworkspace_store::approval_worklog::WorkLogPage,
    actors: &std::collections::BTreeMap<String, String>,
) -> serde_json::Value {
    let events = page
        .events
        .into_iter()
        .filter_map(|event| {
            let actor_ref = actors.get(&event.principal)?;
            Some(json!({
                "id": format!("e_{}", event.event_seq),
                "at": aiworkspace_core::value::format_utc_ms(event.at_ms),
                "actor_ref": actor_ref, "actor_role": event.role, "kind": event.kind,
                "target_label": event.target_label, "summary": event.summary,
            }))
        })
        .collect::<Vec<_>>();
    let mut response = json!({
        "schema_version": 1, "project_id": input.project_id, "version": input.version,
        "workspace_id": source.workspace_id, "surface_id": source.surface_id,
        "cutoff_at": input.cutoff_at, "events": events,
        "next_before": page.next_before.map(|seq| format!("e_{seq}")),
    });
    if let (Some(epoch), Some(seq)) = (input.cutoff_epoch.as_deref(), input.cutoff_event_seq) {
        response["cutoff_epoch"] = json!(epoch);
        response["cutoff_event_seq"] = json!(seq);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    const CHILD: &str = "aiws-26117kzf.buckyos.ncu.bin-go.cc";

    #[test]
    fn approval_capture_parses_only_native_canvas_urls() {
        for id in ["approval-lab_surface", "approval%2Dlab%5Fsurface"] {
            let input = format!("https://{CHILD}/my-workspace/my-canvas?id={id}");
            let parsed = Source::parse(&input).unwrap();
            assert_eq!(parsed.workspace_id, "ws_26117kzf");
            assert_eq!(parsed.surface_id, "approval-lab_surface");
        }
        for bad in [
            format!("http://{CHILD}/ws/surface?id=selected"),
            format!("https://{CHILD}:443/ws/surface?id=selected"),
            format!("https://nobody@{CHILD}/ws/surface?id=selected"),
            format!("https://{CHILD}.elsewhere.example/ws/surface?id=selected"),
            "https://aiws-26AA7kzf.buckyos.ncu.bin-go.cc/ws/surface?id=selected".into(),
            "https://aiws-26137kzf.buckyos.ncu.bin-go.cc/ws/surface?id=selected".into(),
            format!("https://{CHILD}/ws?id=selected"),
            format!("https://{CHILD}/kapi/surface?id=selected"),
            format!("https://{CHILD}/gone/../ws/surface?id=selected"),
            format!("https://{CHILD}/ws/surface?id=%73elected"),
            format!("https://{CHILD}/ws/surface?id=selected#share"),
            format!("https://{CHILD}/ws/surface?id=selected&id=other"),
            format!("https://{CHILD}/ws/surface?id=selected&share=secret"),
            format!("https://{CHILD}/ws/surface?id=selected%2Fother"),
        ] {
            assert!(Source::parse(&bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn approval_capture_accepts_only_the_active_existing_school_binding() {
        let binding = NcuCasLoginRequest {
            identity_digest: "ab".repeat(32),
            user_type: UserType::User,
        };
        let id = binding.user_id().unwrap();
        let mut account: UserSettings = serde_json::from_value(json!({
            "user_id": id, "type": "limited", "password": "", "state": "active",
            "password_state": "unset", "allow_password_change": false, "res_pool_id": "",
            "is_local": true
        }))
        .unwrap();
        let key = serde_json::from_value(json!({
            "kty": "OKP", "crv": "Ed25519", "x": "qJdNEtscIYwTo-I0K7iPEt_UZdBDRd4r16jdBfNR0tM"
        }))
        .unwrap();
        let mut document = OwnerDocument::new(name_lib::DID::new("bns", &id), id, "Test account".into(), key);

        assert!(!is_bound_account(binding.clone(), &account, &document));
        document.extra_info.insert(
            buckyos_api::NCU_CAS_IDENTITY_MARKER.into(),
            json!(binding.identity_digest),
        );
        document.extra_info.insert(NCU_CAS_READY_MARKER.into(), json!(true));
        assert!(is_bound_account(binding.clone(), &account, &document));

        account.state = buckyos_api::UserState::Suspended("lab".into());
        assert!(!is_bound_account(binding.clone(), &account, &document));
        account.state = buckyos_api::UserState::Active;
        account.user_type = UserType::Admin;
        assert!(!is_bound_account(binding.clone(), &account, &document));
        account.user_type = UserType::User;
        document
            .extra_info
            .insert(buckyos_api::NCU_CAS_IDENTITY_MARKER.into(), json!("aa".repeat(32)));
        assert!(!is_bound_account(binding, &account, &document));
    }

    #[test]
    fn approval_capture_requires_exactly_one_dedicated_bearer() {
        let key = "g".repeat(43);
        let mut headers = HeaderMap::new();
        assert!(!authorized(&headers, &key));
        let wrong = HeaderValue::from_str(&format!("Bearer {}", "f".repeat(43))).unwrap();
        headers.insert(header::AUTHORIZATION, wrong);
        assert!(!authorized(&headers, &key));
        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer short"));
        assert!(!authorized(&headers, &key));
        let correct = HeaderValue::from_str(&format!("Bearer {key}")).unwrap();
        headers.insert(header::AUTHORIZATION, correct.clone());
        assert!(authorized(&headers, &key));
        headers.append(header::AUTHORIZATION, correct);
        assert!(!authorized(&headers, &key));
    }
}

#[cfg(test)]
mod paired_work_log_fixture {
    use super::*;
    use aiworkspace_store::{
        approval_worklog::WorkLogQuery, classroom::ClassroomKind, entries::split_entry_id, objects::ObjectStore,
        Caller, Service, Workspace,
    };
    use std::sync::atomic::{AtomicU64, Ordering};

    fn commit(ws: &mut Workspace, caller: &Caller, operations: serde_json::Value) {
        static KEY: AtomicU64 = AtomicU64::new(0);
        let result = ws.commit(
            &json!({
                "protocol_version": aiworkspace_core::model::PROTOCOL_VERSION,
                "workspace_id": ws.workspace_id, "epoch": ws.epoch, "operations": operations,
                "idempotency_key": format!("paired-fixture/{}", KEY.fetch_add(1, Ordering::SeqCst)),
            }),
            caller,
            &Default::default(),
        );
        assert_eq!(result["status"], "accepted", "{result}");
    }

    #[test]
    fn real_work_log_rows_and_manifest_serialize_to_paired_wire() {
        let config = NcuCasConfig {
            app_key: "fixture-app-key".into(),
            app_secret: "fixture-secret".into(),
            identity_key: "42".repeat(32),
            callback_url: Url::parse("https://buckyos.ncu.bin-go.cc/sso/cas/callback").unwrap(),
        };
        let approval_key = "q".repeat(43);
        let school = "fixture-teacher-01";
        let student_id = "fixture-student-01";
        let principal = |school_id: &str| {
            NcuCasLoginRequest {
                identity_digest: ncu_cas_identity_digest(&config.app_key, &config.identity_key, school_id).unwrap(),
                user_type: UserType::Limited,
            }
            .user_id()
            .unwrap()
        };
        let teacher = Caller::verified_account(&principal(school), true, true);
        let student = Caller::verified_account(&principal(student_id), true, false);
        let tmp = tempfile::tempdir().unwrap();
        let objects = ObjectStore::embedded(
            &tmp.path().join("objects"),
            buckyos_api::aiworkspace_bucket_decls(),
            Some(buckyos_api::AIWORKSPACE_STORAGE_QUOTA_MIB),
        )
        .unwrap();
        let svc = Service::open_with_clock(tmp.path(), objects, Arc::new(|| 1_794_959_200_000)).unwrap();
        let created = svc.create_workspace(&teacher, "Fixture classroom", None).unwrap();
        let workspace_id = created["workspace_id"].as_str().unwrap();
        let suffix = workspace_id.strip_prefix("ws_").unwrap();
        let surface = "scope-main";
        let href = format!("https://aiws-{suffix}.buckyos.ncu.bin-go.cc/fixture-classroom/scope-main?id=scope-main");
        let source = Source::parse(&href).unwrap();
        let handle = svc.workspace(workspace_id).unwrap();
        let mut ws = handle.lock().unwrap();
        commit(
            &mut ws,
            &teacher,
            json!([
                {"op":"entity.create","entity_id":"scope-content","parent_id":"canvas-content","order_key":"a",
                 "type_id":"buckyos.container","payload":{"kind":"folder","system":"surface_content","surface_id":surface}},
                {"op":"entity.create","entity_id":surface,"parent_id":"surfaces","order_key":"a",
                 "type_id":"buckyos.container","payload":{"kind":"surface","title":"Scope main","layout":{"mode":"free"},"content_folder_id":"scope-content"}}
            ]),
        );
        let offer = ws
            .classroom_issue(&teacher, &student.principal, surface, ClassroomKind::Student)
            .unwrap();
        ws.classroom_redeem(&student, split_entry_id(offer["entry_id"].as_str().unwrap()).unwrap().1)
            .unwrap();
        let file = b"synthetic approval attachment";
        let staged = ws.stage_asset(&student, file).unwrap();
        let object_id = staged["object_id"].as_str().unwrap();
        commit(
            &mut ws,
            &student,
            json!([
                {"op":"entity.create","entity_id":"frame1","parent_id":surface,"order_key":"a","type_id":"buckyos.cell",
                 "placement":{"x":0,"y":0,"w":320,"h":240},"payload":{"view":{"type":"frame"},"title":"Synthetic student Frame"}},
                {"op":"entity.create","entity_id":"asset1","parent_id":"scope-content","order_key":"a","type_id":"buckyos.asset-ref",
                 "payload":{"object_id":object_id,"file_name":"Synthetic.txt"}},
                {"op":"entity.create","entity_id":"filecell","parent_id":surface,"order_key":"b","type_id":"buckyos.cell",
                 "placement":{"x":360,"y":0,"w":240,"h":180},"payload":{"view":{"type":"asset"},"source_ref":{"entity_id":"asset1"}}}
            ]),
        );
        assert_eq!(ws.download_approval_asset(&student, "asset1").unwrap(), file);
        let reply = |id: &str, order: &str, text: &str| {
            json!({
                "op":"entity.create","entity_id":id,"parent_id":"scope-content","order_key":order,"type_id":"buckyos.annotation",
                "payload":{"kind":"note","target":{"entity_id":"note1"},"body":text}
            })
        };
        commit(
            &mut ws,
            &teacher,
            json!([
                {"op":"entity.create","entity_id":"note1","parent_id":"scope-content","order_key":"b","type_id":"buckyos.annotation",
                 "payload":{"kind":"note","body":"Synthetic original note"}},
                reply("reply1", "c", "Synthetic verified reply.")
            ]),
        );
        let captured = ws.capture_approval_viewer(&teacher.principal, surface).unwrap();
        let cut = captured.work_log_cutoff.clone();
        let at = captured.captured_at.clone();
        let mut canonical = source.url.clone();
        canonical_url(&mut canonical, &ws.workspace_slug(), &captured.title, surface);
        let manifest = viewer_response("paired-log-project", &canonical, workspace_id, surface, captured);
        let live_request = json!({
            "project_id":"paired-log-project","version":2,"school_id":school,"workspace_url":href,
            "participant_school_ids":[student_id],"before":null,"limit":50,"cutoff_at":null
        });
        let page = |ws: &Workspace, payload: serde_json::Value| {
            let input: WorkLogRequest = serde_json::from_value(payload).unwrap();
            let actors = project_actors(&input, &config, &approval_key).unwrap();
            let principals = actors.keys().cloned().collect::<Vec<_>>();
            let query = WorkLogQuery {
                cutoff_ms: input
                    .cutoff_at
                    .as_deref()
                    .and_then(aiworkspace_core::value::parse_datetime_ms),
                cutoff: input.cutoff_epoch.as_deref().zip(input.cutoff_event_seq),
                before: None,
                limit: input.limit,
                principals: &principals,
            };
            let data = ws.approval_work_log(&teacher.principal, surface, &query).unwrap();
            work_log_response(&input, &source, data, &actors)
        };
        let mut archive_request = live_request.clone();
        archive_request["cutoff_epoch"] = json!(cut.epoch);
        archive_request["cutoff_event_seq"] = json!(cut.last_event_seq);
        archive_request["cutoff_at"] = json!(at);
        commit(
            &mut ws,
            &teacher,
            json!([reply("later-reply", "d", "Outside archive cutoff.")]),
        );
        let live_response = page(&ws, live_request.clone());
        let archive_response = page(&ws, archive_request.clone());
        assert_eq!(manifest["assets_complete"], true);
        assert_eq!(manifest["work_log_cutoff"]["epoch"], manifest["epoch"]);
        assert_eq!(live_response["events"].as_array().unwrap().len(), 5);
        assert_eq!(archive_response["events"].as_array().unwrap().len(), 4);
        assert_eq!(archive_response["events"][0]["summary"], "Synthetic verified reply.");
        assert!(archive_response["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row.get("principal").is_none()));
        if let Some(output) = std::env::var_os("AIWS_APPROVAL_WORKLOG_SAMPLE") {
            use base64::Engine;
            std::fs::write(output, serde_json::to_vec_pretty(&json!({
                "approval_key": approval_key, "live_request": live_request, "live_response": live_response,
                "archive_request": archive_request, "archive_response": archive_response, "manifest": manifest,
                "asset": {"object_id":object_id,"media_type":"text/plain","bytes_base64":base64::engine::general_purpose::STANDARD.encode(file)}
            })).unwrap()).unwrap();
        }
    }
}
