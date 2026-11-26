use crate::agent_mgr::ZoneKv;
use crate::ncu_cas::{
    attempt_cookie, configured_cas_origin, ensure_pbl_account, query_param, reply, PendingLogins,
    ReturnTarget,
};
use crate::ControlPanelServer;
use ::kRPC::RPCSessionToken;
use buckyos_api::{
    get_buckyos_api_runtime, is_ncu_handoff_token, load_ncu_pbl_teacher_key, token_auth_method,
    validate_verify_hub_token_claims, AuthMethod, AuthTarget, NcuCasLoginRequest, NcuPblTeacherLoginRequest, UserType,
    TokenPrincipalKind, TokenUse, CONTROL_PANEL_SERVICE_UNIQUE_ID,
};
use buckyos_http_server::{ServerError, ServerResult};
use bytes::Bytes;
use http::{Request, Response, StatusCode};
use http_body_util::combinators::BoxBody;
use std::sync::OnceLock;
use std::time::Instant;
use tokio::sync::Mutex;
use url::Url;

pub(super) const START_PATH: &str = "/sso/pbl/start";
pub(super) const CALLBACK_PATH: &str = "/sso/pbl/callback";
const HANDOFF_PAGE: &str = "https://139.196.234.199/xmz/buckyos-handoff.html";
const COOKIE_PREFIX: &str = "__Host-buckyos-ncu-pbl-";
const WORKSPACE_LIST: &str = "/workspace";
const RESERVED_WORKSPACE_SLUGS: &[&str] = &[
    "home", "kapi", "api", "s3", "bucket", "ndm", "nfs", "assets", "aiworkspace", "messagehub",
];
type Body = BoxBody<Bytes, ServerError>;

fn pending() -> &'static Mutex<PendingLogins> {
    static PENDING: OnceLock<Mutex<PendingLogins>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(PendingLogins::new(COOKIE_PREFIX)))
}

fn enabled(req: &Request<Body>) -> bool {
    configured_cas_origin(req) && load_ncu_pbl_teacher_key(&crate::gateway_etc_dir()).is_some()
}

fn handoff_url(state: &str) -> Url {
    let mut url = Url::parse(HANDOFF_PAGE).expect("fixed PBL handoff URL");
    url.set_fragment(Some(&format!("state={state}")));
    url
}

fn canvas_target(query: Option<&str>, zone: &Url) -> Result<Option<ReturnTarget>, ()> {
    let Some(query) = query.filter(|query| !query.is_empty()) else {
        return Ok(None);
    };
    if query.len() > 2048 {
        return Err(());
    }
    let mut params = url::form_urlencoded::parse(query.as_bytes());
    let Some((name, value)) = params.next() else {
        return Err(());
    };
    if params.next().is_some() {
        return Err(());
    }
    match name.as_ref() {
        "canvas_state" if is_ncu_handoff_token(&value) => {
            let location = format!("{WORKSPACE_LIST}#pbl_canvas={value}");
            ReturnTarget::parse(&location, zone).map(Some).ok_or(())
        }
        "canvas_url" => {
            let (address, id) = value.split_once("?id=").ok_or(())?;
            let alphanumeric = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
            let valid_id = id.len() <= 64
                && id.bytes().next().is_some_and(alphanumeric)
                && id.bytes().all(|byte| alphanumeric(byte) || matches!(byte, b'-' | b'_'));
            let (host, path) = address
                .strip_prefix("https://")
                .and_then(|address| address.split_once('/'))
                .ok_or(())?;
            let zone_suffix = format!(".{}", zone.domain().ok_or(())?);
            let label = host.strip_suffix(&zone_suffix).and_then(|host| host.strip_prefix("aiws-")).ok_or(())?;
            let valid_host = label.len() == 8
                && label.bytes().take(2).all(|byte| byte.is_ascii_digit())
                && label.get(2..4).and_then(|month| month.parse::<u8>().ok()).is_some_and(|month| (1..=12).contains(&month))
                && label.bytes().all(alphanumeric);
            let mut slugs = path.split('/');
            let valid_slug = |slug: &str| {
                !slug.is_empty() && slug.len() <= 64
                    && slug.bytes().all(|byte| alphanumeric(byte) || byte == b'-')
            };
            let valid_workspace_slug =
                |slug: &str| valid_slug(slug) && !RESERVED_WORKSPACE_SLUGS.contains(&slug);
            if !valid_host || !valid_id
                || !slugs.next().is_some_and(valid_workspace_slug)
                || !slugs.next().is_some_and(valid_slug)
                || slugs.next().is_some()
            {
                return Err(());
            }
            Url::parse(&value).map(|url| Some(ReturnTarget::Workspace(url))).map_err(|_| ())
        }
        _ => Err(()),
    }
}

fn failed(reason: &str, state: Option<&str>) -> ServerResult<Response<Body>> {
    reply(
        StatusCode::SEE_OTHER,
        Some(&format!("/login?cas_error={reason}")),
        state
            .into_iter()
            .map(|state| attempt_cookie(COOKIE_PREFIX, state, None))
            .collect(),
    )
}

fn verified_new_subject<'a>(token: &'a RPCSessionToken, target: &AuthTarget) -> Option<&'a str> {
    let claims = validate_verify_hub_token_claims(token, TokenUse::Session).ok()?;
    if token.sudo
        || token_auth_method(token) != Some(AuthMethod::NcuPblTeacher)
        || claims.principal_kind != TokenPrincipalKind::User
        || &claims.target != target
    {
        return None;
    }
    token.sub.as_deref().filter(|s| !s.is_empty())
}

impl ControlPanelServer {
    pub(super) async fn serve_ncu_pbl_start(
        &self,
        req: Request<Body>,
    ) -> ServerResult<Response<Body>> {
        if !enabled(&req) {
            return reply(StatusCode::NOT_FOUND, None, vec![]);
        }
        let Some(zone) = Self::request_origin(&req, false, 0)
            .ok()
            .and_then(|origin| Url::parse(&origin).ok())
        else {
            return reply(StatusCode::BAD_REQUEST, None, vec![]);
        };
        let Ok(target) = canvas_target(req.uri().query(), &zone) else {
            return reply(StatusCode::BAD_REQUEST, None, vec![]);
        };
        let Some((state, browser)) = pending().lock().await.begin(Instant::now(), &req, target)
        else {
            return failed("unavailable", None);
        };
        reply(
            StatusCode::SEE_OTHER,
            Some(handoff_url(&state).as_str()),
            vec![attempt_cookie(COOKIE_PREFIX, &state, Some(&browser))],
        )
    }

    pub(super) async fn serve_ncu_pbl_callback(
        &self,
        req: Request<Body>,
    ) -> ServerResult<Response<Body>> {
        if !enabled(&req) {
            return reply(StatusCode::NOT_FOUND, None, vec![]);
        }
        let Some((state, return_target)) = pending().lock().await.consume(&req, Instant::now()) else {
            return failed("invalid_callback", None);
        };
        let failure = |reason| failed(reason, Some(&state));
        let query = req.uri().query().unwrap_or_default();
        let Some(code) = query_param(query, "code", 43).filter(|s| is_ncu_handoff_token(s)) else {
            return failure("provider_denied");
        };
        let Ok(runtime) = get_buckyos_api_runtime() else {
            return failure("unavailable");
        };
        let Ok(verify_hub) = runtime.get_verify_hub_client().await else {
            return failure("unavailable");
        };
        let Ok(service_id) = CONTROL_PANEL_SERVICE_UNIQUE_ID.parse() else {
            return failure("unavailable");
        };
        let target = AuthTarget::system(service_id);
        let Ok(enrollment) = verify_hub
            .prepare_ncu_pbl_teacher_enrollment(NcuPblTeacherLoginRequest { state: state.clone(), code })
            .await
        else {
            return failure("account_unavailable");
        };
        let binding = NcuCasLoginRequest {
            identity_digest: enrollment.identity_digest,
            user_type: UserType::Limited,
        };
        let Some(expected_subject) = binding.user_id() else {
            return failure("account_unavailable");
        };
        if Self::verified_pbl_browser_conflict(&req, runtime, &target, &expected_subject).await {
            return failure("identity_conflict");
        }
        let account = async {
            let kv = ZoneKv::connect().await?;
            ensure_pbl_account(&kv, &runtime.zone_id, &binding.identity_digest, enrollment.new_account_type).await
        }.await;
        if !account.is_ok_and(|id| id == expected_subject) {
            log::warn!("NCU PBL account enrollment refused");
            return failure("account_unavailable");
        }
        let Ok(session) = verify_hub.finish_ncu_pbl_teacher_enrollment(enrollment.enrollment_id).await else {
            return failure("account_unavailable");
        };
        let subject =
            Self::validate_sso_token_pair(&session.session_token, &session.refresh_token, &target)
                .ok()
                .and_then(|()| RPCSessionToken::from_string(&session.session_token).ok());
        if subject.as_ref().and_then(|token| verified_new_subject(token, &target))
            != Some(expected_subject.as_str())
        {
            let _ = verify_hub.logout(&session.refresh_token).await;
            return failure("account_unavailable");
        }
        let mut cookies = match Self::build_sso_cookie_headers(
            &req,
            &session.session_token,
            &session.refresh_token,
        ) {
            Ok(cookies) => cookies,
            Err(_) => {
                let _ = verify_hub.logout(&session.refresh_token).await;
                return failure("account_unavailable");
            }
        };
        cookies.push(attempt_cookie(COOKIE_PREFIX, &state, None));
        let location = return_target.map_or_else(|| WORKSPACE_LIST.to_owned(), |target| target.success_location());
        reply(StatusCode::SEE_OTHER, Some(&location), cookies)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::header::{COOKIE, SET_COOKIE};

    #[test]
    fn pbl_handoff_has_fixed_origin_and_state_only_in_fragment() {
        let state = "r".repeat(43);
        let url = handoff_url(&state);
        assert_eq!(
            url.origin().ascii_serialization(),
            "https://139.196.234.199"
        );
        assert_eq!(url.path(), "/xmz/buckyos-handoff.html");
        assert!(url.query().is_none());
        assert_eq!(url.fragment(), Some(format!("state={state}").as_str()));
    }

    #[test]
    fn pbl_canvas_selection_and_direct_open_stay_on_approved_routes() {
        let zone = Url::parse("https://buckyos.ncu.bin-go.cc").unwrap();
        let state = "z".repeat(43);
        assert_eq!(canvas_target(None, &zone), Ok(None));
        let chooser = canvas_target(Some(&format!("canvas_state={state}")), &zone).unwrap().unwrap();
        assert_eq!(chooser.success_location(), format!("{WORKSPACE_LIST}#pbl_canvas={state}"));
        let query = |url: &str| url::form_urlencoded::Serializer::new(String::new()).append_pair("canvas_url", url).finish();
        for slug in ["workspace", "login", "sso"] {
            let valid = format!("https://aiws-26116eo2.buckyos.ncu.bin-go.cc/{slug}/surface?id=surface-1");
            let direct = canvas_target(Some(&query(&valid)), &zone).unwrap().unwrap();
            let redirect = zone.join(&direct.success_location()).unwrap();
            assert_eq!(redirect.path(), "/login");
            assert_eq!(redirect.query_pairs().collect::<Vec<_>>(), [("redirect_url".into(), valid.into())]);
        }
        for url in [
            "https://example.com/workspace/surface?id=surface-1",
            "https://buckyos.ncu.bin-go.cc/workspace/surface?id=surface-1",
            "https://aiws-26116eo2.buckyos.ncu.bin-go.cc/kapi/surface?id=surface-1",
            "https://aiws-26116eo2.buckyos.ncu.bin-go.cc/workspace/surface?id=surface-1&entry=secret",
            "https://aiws-26116eo2.buckyos.ncu.bin-go.cc/workspace/surface?id=surface-1#pbl_canvas=secret",
            "https://aiws-26116eo2.buckyos.ncu.bin-go.cc/sso/cas/start?id=surface-1",
            "https://aiws-26116eo2.buckyos.ncu.bin-go.cc:443/workspace/surface?id=surface-1",
            "https://aiws-26136eo2.buckyos.ncu.bin-go.cc/workspace/surface?id=surface-1",
        ] {
            assert!(canvas_target(Some(&query(url)), &zone).is_err(), "{url}");
        }
        assert!(canvas_target(Some("canvas_state=short"), &zone).is_err());
        assert!(canvas_target(Some(&format!("canvas_state={state}&canvas_state={state}")), &zone).is_err());
    }

    #[test]
    fn pbl_attempt_requires_its_own_browser_cookie_and_rejects_replay() {
        let empty = Request::new(ControlPanelServer::boxed_http_body(Vec::new()));
        let now = Instant::now();
        let mut pbl = PendingLogins::new(COOKIE_PREFIX);
        let mut cas = PendingLogins::default();
        let zone = Url::parse("https://buckyos.ncu.bin-go.cc").unwrap();
        let choice = "z".repeat(43);
        let target = canvas_target(Some(&format!("canvas_state={choice}")), &zone).unwrap();
        let (state, browser) = pbl.begin(now, &empty, target.clone()).unwrap();
        let request = |cookie| {
            Request::builder()
                .uri(format!("{CALLBACK_PATH}?state={state}"))
                .header(COOKIE, cookie)
                .body(ControlPanelServer::boxed_http_body(Vec::new()))
                .unwrap()
        };
        let cas_cookie = request(format!("__Host-buckyos-ncu-cas-{state}={browser}"));
        let pbl_cookie = request(format!("{COOKIE_PREFIX}{state}={browser}"));
        assert!(pbl.consume(&cas_cookie, now).is_none());
        assert!(cas.consume(&pbl_cookie, now).is_none());
        assert_eq!(pbl.consume(&pbl_cookie, now), Some((state, target)));
        assert!(pbl.consume(&pbl_cookie, now).is_none());
        assert!(!failed("invalid_callback", None)
            .unwrap()
            .headers()
            .contains_key(SET_COOKIE));
    }
}
