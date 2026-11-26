use crate::agent_mgr::{AgentKv, AgentNameIndex, ZoneKv};
use crate::exclusive_invite::random_token;
use crate::user_mgr::{after_account_created, new_account_actions};
use crate::ControlPanelServer;
use ::kRPC::{RPCErrors, RPCSessionToken};
use buckyos_api::{
    get_buckyos_api_runtime, is_aiworkspace_host_label, is_ncu_handoff_token,
    ncu_cas_identity_digest, ncu_school_user_type, AuthTarget, NcuCasConfig as Config, NcuCasLoginRequest, PasswordState,
    UserSettings, UserState, UserType, AIWORKSPACE_UNIQUE_ID, CONTROL_PANEL_SERVICE_UNIQUE_ID,
    NCU_CAS_CONFIG_FILE, NCU_CAS_IDENTITY_MARKER, NCU_CAS_READY_MARKER,
};
use buckyos_http_server::{server_err, ServerError, ServerErrorCode, ServerResult};
use buckyos_kit::KVAction;
use bytes::Bytes;
use http::header::{CACHE_CONTROL, COOKIE, LOCATION, REFERRER_POLICY, SET_COOKIE};
use http::{Request, Response, StatusCode};
use http_body_util::combinators::BoxBody;
use name_lib::{OwnerDocument, DID};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use url::{form_urlencoded, Position, Url};

pub(crate) const START_PATH: &str = "/sso/cas/start";
pub(crate) const CALLBACK_PATH: &str = buckyos_api::NCU_CAS_CALLBACK_PATH;
const IAADP_TOKEN: &str = "https://iaa.ncu.edu.cn/openapi/v1/auth/sso/token";
const BROWSER_COOKIE_PREFIX: &str = "__Host-buckyos-ncu-cas-";
const LOGIN_TTL: Duration = Duration::from_secs(600);
const MAX_PENDING: usize = 4096;
const MAX_BROWSER_PENDING: usize = 8;
const MAX_REDIRECT_URL: usize = 2048;
const MAX_TOKEN_RESPONSE: usize = 64 * 1024;
const MAX_QUERY_LEN: usize = 8192;
type Body = BoxBody<Bytes, ServerError>;

fn load_config() -> Option<Config> {
    let file = match std::fs::File::open(crate::gateway_etc_dir().join(NCU_CAS_CONFIG_FILE)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => {
            log::warn!("NCU CAS configuration could not be read");
            return None;
        }
    };
    let config = Config::read(file);
    if config.is_none() {
        log::warn!("NCU CAS configuration is invalid or has unsafe permissions");
    }
    config
}

fn correct_origin(req: &Request<Body>, config: &Config) -> bool {
    ControlPanelServer::request_origin(req, false, 0)
        .is_ok_and(|origin| origin == config.callback_url.origin().ascii_serialization())
}

pub(super) fn configured_cas_origin(req: &Request<Body>) -> bool {
    CasBridge::get().is_some_and(|bridge| correct_origin(req, &bridge.config))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ReturnTarget {
    Zone(Url),
    Workspace(Url),
}

impl ReturnTarget {
    pub(super) fn parse(redirect_url: &str, callback_url: &Url) -> Option<Self> {
        if redirect_url.len() > MAX_REDIRECT_URL
            || redirect_url.is_empty()
            || redirect_url.contains('\\')
            || redirect_url
                .chars()
                .any(|ch| ch.is_whitespace() || ch.is_control())
        {
            return None;
        }
        let url = if redirect_url.starts_with('/') && !redirect_url.starts_with("//") {
            callback_url.join(redirect_url).ok()?
        } else {
            // Require ordinary absolute HTTPS syntax. The URL parser accepts and
            // normalizes unusual forms (including an empty username and '@').
            let scheme = redirect_url.get(..8)?;
            let authority = redirect_url.get(8..)?.split(['/', '?', '#']).next()?;
            if !scheme.eq_ignore_ascii_case("https://")
                || authority.is_empty()
                || authority.contains('@')
            {
                return None;
            }
            Url::parse(redirect_url).ok()?
        };
        if url.as_str().len() > MAX_REDIRECT_URL
            || url.scheme() != "https"
            || url.port().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return None;
        }

        let same_origin = url.origin() == callback_url.origin();
        let path = url.path().to_ascii_lowercase();
        let route = path.trim_end_matches('/');
        let segment = route.split('/').nth(1).unwrap_or_default();
        if path.starts_with("//")
            || segment.contains('%')
            || ["%2f", "%5c", "%25"]
                .iter()
                .any(|escape| path.contains(escape))
            || segment == "login"
            || segment.starts_with("sso_")
            || (segment == "sso" && !(same_origin && route == "/sso/entry"))
        {
            return None;
        }
        if same_origin {
            return Some(Self::Zone(url));
        }
        let zone_suffix = format!(".{}", callback_url.domain()?);
        let label = url.domain()?.strip_suffix(&zone_suffix)?;
        (label.len() <= 63 && is_aiworkspace_host_label(label)).then_some(Self::Workspace(url))
    }

    fn url(&self) -> &Url {
        match self {
            Self::Zone(url) | Self::Workspace(url) => url,
        }
    }

    pub(super) fn success_location(&self) -> String {
        match self {
            Self::Zone(url) => url[Position::BeforePath..].to_owned(),
            Self::Workspace(_) => login_location(None, Some(self)),
        }
    }
}

struct PendingLogin {
    browser: String,
    target: Option<ReturnTarget>,
    deadline: Instant,
}

pub(super) struct PendingLogins {
    cookie_prefix: &'static str,
    attempts: HashMap<String, PendingLogin>,
}

impl Default for PendingLogins {
    fn default() -> Self {
        Self::new(BROWSER_COOKIE_PREFIX)
    }
}

impl PendingLogins {
    pub(super) fn new(cookie_prefix: &'static str) -> Self {
        Self {
            cookie_prefix,
            attempts: HashMap::new(),
        }
    }

    pub(super) fn begin(
        &mut self,
        now: Instant,
        req: &Request<Body>,
        target: Option<ReturnTarget>,
    ) -> Option<(String, String)> {
        self.attempts.retain(|_, attempt| attempt.deadline > now);
        if self.attempts.len() >= MAX_PENDING
            || cookie_pairs(req)
                .filter_map(|(name, _)| name.strip_prefix(self.cookie_prefix))
                .take(MAX_BROWSER_PENDING)
                .count()
                >= MAX_BROWSER_PENDING
        {
            return None;
        }
        // Separate cookies let even two initial, simultaneous tabs complete
        // independently. A callback may remove only its own cookie and state.
        let state = random_token(32);
        let browser = random_token(32);
        self.attempts.insert(
            state.clone(),
            PendingLogin {
                browser: browser.clone(),
                target,
                deadline: now + LOGIN_TTL,
            },
        );
        Some((state, browser))
    }

    pub(super) fn consume(
        &mut self,
        req: &Request<Body>,
        now: Instant,
    ) -> Option<(String, Option<ReturnTarget>)> {
        let query = req
            .uri()
            .query()
            .filter(|query| query.len() <= MAX_QUERY_LEN)?;
        let state = query_param(query, "state", 43).filter(|state| is_ncu_handoff_token(state))?;
        let cookie_name = format!("{}{state}", self.cookie_prefix);
        let mut values =
            cookie_pairs(req).filter_map(|(name, value)| (name == cookie_name).then_some(value));
        let browser = values.next()?;
        if !is_ncu_handoff_token(browser) || values.next().is_some() {
            return None;
        }
        let attempt = self.attempts.get(&state)?;
        if attempt.browser != browser {
            return None;
        }
        let attempt = self.attempts.remove(&state)?;
        (attempt.deadline > now).then_some((state, attempt.target))
    }
}

struct CasBridge {
    config: Config,
    http: reqwest::Client,
    pending: Mutex<PendingLogins>,
}

impl CasBridge {
    fn get() -> Option<&'static Self> {
        static BRIDGE: OnceLock<Option<CasBridge>> = OnceLock::new();
        BRIDGE
            .get_or_init(|| {
                let config = load_config()?;
                let http = reqwest::Client::builder()
                    .https_only(true)
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(Duration::from_secs(8))
                    .build()
                    .ok()?;
                Some(Self {
                    config,
                    http,
                    pending: Mutex::default(),
                })
            })
            .as_ref()
    }

    async fn exchange(&self, code: &str) -> Result<ProviderUser, &'static str> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "unavailable")?
            .as_millis()
            .to_string();
        let nonce = uuid::Uuid::new_v4().to_string();
        let signature = self
            .config
            .signature(&timestamp, &nonce)
            .ok_or("unavailable")?;
        let mut response = self
            .http
            .post(IAADP_TOKEN)
            .header("X-Open-AppKey", &self.config.app_key)
            .header("X-Open-Timestamp", timestamp)
            .header("X-Open-Nonce", nonce)
            .header("X-Open-Signature", signature)
            .json(&json!({ "code": code }))
            .send()
            .await
            .map_err(|_| provider_unavailable("transport"))?;
        let status = response.status();
        if !status.is_success() {
            log::warn!("NCU CAS token exchange failed: HTTP {}", status.as_u16());
            return Err("provider_unavailable");
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| provider_unavailable("body_read"))?
        {
            if body.len() + chunk.len() > MAX_TOKEN_RESPONSE {
                return Err(provider_unavailable("body_too_large"));
            }
            body.extend_from_slice(&chunk);
        }
        parse_provider_user(&body)
    }
}

fn provider_unavailable(category: &'static str) -> &'static str {
    log::warn!("NCU CAS token exchange failed: {category}");
    "provider_unavailable"
}

#[derive(Deserialize)]
struct ProviderTokenResponse {
    code: ProviderResultCode,
    data: Option<Value>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ProviderResultCode {
    Number(u32),
    Decimal(String),
}

impl ProviderResultCode {
    fn number(self) -> Option<u32> {
        match self {
            Self::Number(code) => Some(code),
            Self::Decimal(code) => code
                .parse::<u32>()
                .ok()
                .filter(|number| number.to_string() == code),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderUser {
    user_id: String,
    user_type: String,
    #[serde(skip)]
    dossier: Value,
}

fn parse_provider_user(body: &[u8]) -> Result<ProviderUser, &'static str> {
    let result: ProviderTokenResponse =
        serde_json::from_slice(body).map_err(|_| provider_unavailable("envelope_decode"))?;
    let code = result
        .code
        .number()
        .ok_or_else(|| provider_unavailable("envelope_code"))?;
    if code != 200 {
        return Err("provider_denied");
    }
    let data = result.data.ok_or("provider_denied")?;
    let dossier = crate::ncu_school::verified_dossier(&data);
    let mut user: ProviderUser =
        serde_json::from_value(data).map_err(|_| provider_unavailable("profile_decode"))?;
    user.dossier = dossier;
    Ok(user)
}

impl ProviderUser {
    fn login_request(&self, app_key: &str, identity_key: &str) -> Option<NcuCasLoginRequest> {
        let user_type = ncu_school_user_type(&self.user_type)?;
        Some(NcuCasLoginRequest {
            identity_digest: ncu_cas_identity_digest(app_key, identity_key, &self.user_id)?,
            user_type,
        })
    }
}

fn binding_conflict() -> RPCErrors {
    RPCErrors::NoPermission("NCU CAS account binding conflict".to_owned())
}

struct BoundAccount {
    doc_version: u64,
    public_key: String,
    rbac_ready: bool,
}

async fn read_binding(
    kv: &dyn AgentKv,
    doc_key: &str,
    request: &NcuCasLoginRequest,
) -> Result<Option<BoundAccount>, RPCErrors> {
    let Some((raw_doc, doc_version)) = kv.get(doc_key).await? else {
        return Ok(None);
    };
    let doc: OwnerDocument = serde_json::from_str(&raw_doc).map_err(|_| binding_conflict())?;
    let public_key = request.account_key(&doc).ok_or_else(binding_conflict)?;
    let rbac_ready = doc
        .extra_info
        .get(NCU_CAS_READY_MARKER)
        .and_then(serde_json::Value::as_bool)
        == Some(true);
    Ok(Some(BoundAccount {
        doc_version,
        public_key,
        rbac_ready,
    }))
}

#[derive(PartialEq, Eq)]
enum AccountEnrollment {
    Created,
    Existing,
}

async fn enroll(
    kv: &dyn AgentKv,
    zone: &DID,
    request: &NcuCasLoginRequest,
    user_id: &str,
) -> Result<AccountEnrollment, RPCErrors> {
    if request.user_id().as_deref() != Some(user_id) {
        return Err(binding_conflict());
    }
    let doc_key = format!("users/{user_id}/doc");
    if read_binding(kv, &doc_key, request).await?.is_some() {
        return Ok(AccountEnrollment::Existing);
    }

    let mut names = AgentNameIndex::load(kv, zone).await?;
    names.allow_campus_desktop_namespace(kv, user_id).await?;
    if names.check(user_id).is_err() {
        return read_binding(kv, &doc_key, request)
            .await?
            .map(|_| AccountEnrollment::Existing)
            .ok_or_else(binding_conflict);
    }
    let settings = UserSettings {
        user_id: user_id.to_owned(),
        user_type: request.user_type,
        password: String::new(),
        state: UserState::Active,
        res_pool_id: "default".to_owned(),
        is_local: true,
        allow_password_change: Some(false),
        allow_contact_edit: None,
        groups: Vec::new(),
        password_state: PasswordState::Unset,
    };
    let show_name = format!("南昌大学 · {}", &user_id[4..12]);
    let (mut actions, _) = new_account_actions(&settings, &show_name, zone, false)?;
    let Some(KVAction::Create(raw_doc)) = actions.get_mut(&doc_key) else {
        return Err(binding_conflict());
    };
    let mut doc: OwnerDocument = serde_json::from_str(raw_doc).map_err(|_| binding_conflict())?;
    doc.extra_info.insert(
        NCU_CAS_IDENTITY_MARKER.into(),
        json!(request.identity_digest),
    );
    *raw_doc = serde_json::to_string(&doc).map_err(|_| binding_conflict())?;

    match kv.exec_tx(actions, None).await {
        Ok(()) => Ok(AccountEnrollment::Created),
        Err(error) => read_binding(kv, &doc_key, request)
            .await?
            .map(|_| AccountEnrollment::Existing)
            .ok_or(error),
    }
}

async fn ensure_account_ready(
    kv: &dyn AgentKv,
    request: &NcuCasLoginRequest,
    user_id: &str,
) -> Result<(), RPCErrors> {
    let doc_key = format!("users/{user_id}/doc");
    let binding = read_binding(kv, &doc_key, request)
        .await?
        .ok_or_else(binding_conflict)?;
    if binding.rbac_ready {
        return Ok(());
    }

    after_account_created(user_id, "NCU CAS account enrollment").await?;
    let actions = HashMap::from([(
        doc_key.clone(),
        KVAction::SetByJsonPath(HashMap::from([(
            NCU_CAS_READY_MARKER.to_string(),
            Some(json!(true)),
        )])),
    )]);
    match kv
        .exec_tx(actions, Some((doc_key.clone(), binding.doc_version)))
        .await
    {
        Ok(()) => Ok(()),
        Err(error) => {
            let checked = read_binding(kv, &doc_key, request).await?;
            if checked.is_some_and(|current| {
                current.rbac_ready && current.public_key == binding.public_key
            }) {
                Ok(())
            } else {
                Err(error)
            }
        }
    }
}

async fn read_pbl_account(
    kv: &dyn AgentKv,
    request: &NcuCasLoginRequest,
    user_id: &str,
) -> Result<Option<NcuCasLoginRequest>, RPCErrors> {
    let settings_key = format!("users/{user_id}/settings");
    let doc_key = format!("users/{user_id}/doc");
    let mut settings = kv.get(&settings_key).await?;
    let binding = read_binding(kv, &doc_key, request).await?;
    if binding.is_some() && settings.is_none() {
        // The atomic account creation may have committed between the two reads.
        settings = kv.get(&settings_key).await?;
    }
    match (settings, binding) {
        (None, None) => Ok(None),
        (Some((raw_settings, _)), Some(_)) => {
            let settings: UserSettings = serde_json::from_str(&raw_settings).map_err(|_| binding_conflict())?;
            let existing = NcuCasLoginRequest {
                user_type: settings.user_type,
                ..request.clone()
            };
            existing.matches_account(&settings).then_some(Some(existing)).ok_or_else(binding_conflict)
        }
        _ => Err(binding_conflict()),
    }
}

async fn prepare_pbl_account(
    kv: &dyn AgentKv,
    zone: &DID,
    identity_digest: &str,
    new_account_type: Option<UserType>,
) -> Result<(String, NcuCasLoginRequest), RPCErrors> {
    let mut request = NcuCasLoginRequest {
        identity_digest: identity_digest.to_owned(),
        user_type: UserType::Limited,
    };
    let user_id = request.user_id().ok_or_else(binding_conflict)?;
    let existing = match read_pbl_account(kv, &request, &user_id).await? {
        Some(existing) => existing,
        None => {
            request.user_type = new_account_type
                .filter(|kind| matches!(kind, UserType::User | UserType::Limited))
                .ok_or_else(binding_conflict)?;
            if enroll(kv, zone, &request, &user_id).await? == AccountEnrollment::Created {
                log::info!("NCU PBL account records created for {user_id}");
            }
            read_pbl_account(kv, &request, &user_id).await?.ok_or_else(binding_conflict)?
        }
    };
    Ok((user_id, existing))
}

pub(super) async fn ensure_pbl_account(
    kv: &dyn AgentKv,
    zone: &DID,
    identity_digest: &str,
    new_account_type: Option<UserType>,
) -> Result<String, RPCErrors> {
    let (user_id, existing) = prepare_pbl_account(kv, zone, identity_digest, new_account_type).await?;
    ensure_account_ready(kv, &existing, &user_id).await?;
    Ok(user_id)
}

pub(super) fn query_param<'a>(query: &'a str, name: &str, max: usize) -> Option<String> {
    let mut values = url::form_urlencoded::parse(query.as_bytes())
        .filter_map(|(key, value)| (key == name).then_some(value));
    let value = values.next()?;
    (!value.is_empty()
        && value.len() <= max
        && !value
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
        && values.next().is_none())
    .then(|| value.into_owned())
}

fn cookie_pairs(req: &Request<Body>) -> impl Iterator<Item = (&str, &str)> {
    req.headers()
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
}

fn browser_cookie(state: &str, value: Option<&str>) -> String {
    attempt_cookie(BROWSER_COOKIE_PREFIX, state, value)
}

pub(super) fn attempt_cookie(prefix: &str, state: &str, value: Option<&str>) -> String {
    let max_age = value.map_or(0, |_| LOGIN_TTL.as_secs());
    format!(
        "{prefix}{state}={}; Path=/; Max-Age={max_age}; Secure; HttpOnly; SameSite=Lax",
        value.unwrap_or_default()
    )
}

fn login_location(reason: Option<&str>, target: Option<&ReturnTarget>) -> String {
    let mut query = form_urlencoded::Serializer::new(String::new());
    if let Some(reason) = reason {
        query.append_pair("cas_error", reason);
    }
    if let Some(target) = target {
        query.append_pair("redirect_url", target.url().as_str());
    }
    let query = query.finish();
    if query.is_empty() {
        "/login".to_owned()
    } else {
        format!("/login?{query}")
    }
}

fn return_target(req: &Request<Body>, callback_url: &Url) -> Option<ReturnTarget> {
    let query = req
        .uri()
        .query()
        .filter(|query| query.len() <= MAX_QUERY_LEN)?;
    let value = query_param(query, "redirect_url", MAX_REDIRECT_URL)?;
    let target = ReturnTarget::parse(&value, callback_url)?;
    if matches!(target, ReturnTarget::Workspace(_)) {
        let runtime = get_buckyos_api_runtime().ok()?;
        let route = ControlPanelServer::resolve_sso_auth_target(
            target.url().as_str(),
            &ControlPanelServer::sso_zone_host(runtime).ok()?,
            false,
            runtime.node_gateway_port,
        )
        .ok()?;
        if !matches!(route.auth_target, AuthTarget::System { service_id }
            if service_id.as_str() == AIWORKSPACE_UNIQUE_ID)
        {
            return None;
        }
    }
    Some(target)
}

pub(super) fn reply(
    status: StatusCode,
    location: Option<&str>,
    cookies: Vec<String>,
) -> ServerResult<Response<Body>> {
    let mut response = Response::builder()
        .status(status)
        .header(CACHE_CONTROL, "no-store")
        .header(REFERRER_POLICY, "no-referrer");
    if let Some(location) = location {
        response = response.header(LOCATION, location);
    }
    for cookie in cookies {
        response = response.header(SET_COOKIE, cookie);
    }
    response
        .body(ControlPanelServer::boxed_http_body(Vec::new()))
        .map_err(|_| server_err!(ServerErrorCode::InvalidData, "NCU CAS response"))
}

fn failed(
    reason: &'static str,
    target: Option<&ReturnTarget>,
    consumed_state: Option<&str>,
) -> ServerResult<Response<Body>> {
    reply(
        StatusCode::SEE_OTHER,
        Some(&login_location(Some(reason), target)),
        consumed_state
            .into_iter()
            .map(|state| browser_cookie(state, None))
            .collect(),
    )
}

impl ControlPanelServer {
    pub(crate) async fn serve_ncu_cas_start(
        &self,
        req: Request<Body>,
    ) -> ServerResult<Response<Body>> {
        let Some(bridge) = CasBridge::get().filter(|bridge| correct_origin(&req, &bridge.config))
        else {
            return reply(StatusCode::NOT_FOUND, None, vec![]);
        };
        let target = return_target(&req, &bridge.config.callback_url);
        let Some((state, browser)) =
            bridge
                .pending
                .lock()
                .await
                .begin(Instant::now(), &req, target.clone())
        else {
            return failed("unavailable", target.as_ref(), None);
        };
        reply(
            StatusCode::SEE_OTHER,
            Some(bridge.config.authorize_url(&state).as_str()),
            vec![browser_cookie(&state, Some(&browser))],
        )
    }

    pub(crate) async fn serve_ncu_cas_callback(
        &self,
        req: Request<Body>,
    ) -> ServerResult<Response<Body>> {
        let Some(bridge) = CasBridge::get().filter(|bridge| correct_origin(&req, &bridge.config))
        else {
            return reply(StatusCode::NOT_FOUND, None, vec![]);
        };
        let Some((state, target)) = bridge.pending.lock().await.consume(&req, Instant::now())
        else {
            return failed("invalid_callback", None, None);
        };
        let query = req.uri().query().unwrap_or_default();
        let failure = |reason| failed(reason, target.as_ref(), Some(&state));
        let Some(code) = query_param(query, "code", 1024) else {
            return failure("provider_denied");
        };
        let provider_user = match bridge.exchange(&code).await {
            Ok(user) => user,
            Err(reason) => return failure(reason),
        };
        let Some(request) =
            provider_user.login_request(&bridge.config.app_key, &bridge.config.identity_key)
        else {
            return failure("ineligible");
        };
        let Some(user_id) = request.user_id() else {
            return failure("ineligible");
        };
        if Self::sso_cookie_subjects(&req)
            .iter()
            .any(|held| held != &user_id)
        {
            return failure("identity_conflict");
        }

        let verified_at = chrono::Utc::now().timestamp_millis();
        let result = async {
            let runtime = get_buckyos_api_runtime()?;
            let kv = ZoneKv::connect().await?;
            enroll(&kv, &runtime.zone_id, &request, &user_id).await?;
            ensure_account_ready(&kv, &request, &user_id).await?;
            let verify_hub = runtime.get_verify_hub_client().await?;
            let session = verify_hub.login_by_ncu_cas(request).await?;
            let service_id = CONTROL_PANEL_SERVICE_UNIQUE_ID.parse().map_err(|_| {
                RPCErrors::ReasonError("Control Panel service identifier is invalid".to_owned())
            })?;
            let target = AuthTarget::system(service_id);
            let valid = Self::validate_sso_token_pair(
                &session.session_token,
                &session.refresh_token,
                &target,
            )
            .and_then(|_| RPCSessionToken::from_string(&session.session_token))
            .is_ok_and(|parsed| parsed.sub.as_deref() == Some(user_id.as_str()));
            if !valid {
                let _ = verify_hub.logout(&session.refresh_token).await;
                return Err(RPCErrors::InvalidToken(
                    "NCU CAS session mismatch".to_owned(),
                ));
            }
            if crate::ncu_school::save_verified(&user_id, provider_user.dossier, verified_at)
                .await
                .is_err()
            {
                let _ = verify_hub.logout(&session.refresh_token).await;
                log::warn!("NCU CAS school dossier capture failed");
                return Err(RPCErrors::ReasonError(
                    "NCU school dossier unavailable".into(),
                ));
            }
            Ok(session)
        }
        .await;

        let session = match result {
            Ok(session) => session,
            Err(_) => {
                log::warn!("NCU CAS account or session issuance refused");
                return failure("account_unavailable");
            }
        };
        let mut cookies =
            Self::build_sso_cookie_headers(&req, &session.session_token, &session.refresh_token)?;
        cookies.push(browser_cookie(&state, None));
        let location = target
            .as_ref()
            .map_or_else(|| "/".to_owned(), ReturnTarget::success_location);
        reply(StatusCode::SEE_OTHER, Some(&location), cookies)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_IDENTITY_KEY: &str =
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[derive(Default)]
    struct EnrollmentKv(std::sync::Mutex<HashMap<String, (String, u64)>>);

    #[async_trait::async_trait]
    impl AgentKv for EnrollmentKv {
        async fn get(&self, key: &str) -> Result<Option<(String, u64)>, RPCErrors> {
            Ok(self.0.lock().unwrap().get(key).cloned())
        }

        async fn list(&self, key: &str) -> Result<Vec<String>, RPCErrors> {
            let prefix = format!("{key}/");
            let children: std::collections::BTreeSet<_> = self.0.lock().unwrap().keys()
                .filter_map(|key| key.strip_prefix(&prefix)?.split('/').next())
                .map(str::to_owned)
                .collect();
            Ok(children.into_iter().collect())
        }

        async fn exec_tx(
            &self,
            actions: HashMap<String, KVAction>,
            main_key: Option<(String, u64)>,
        ) -> Result<(), RPCErrors> {
            assert!(main_key.is_none());
            tokio::task::yield_now().await;
            let mut values = self.0.lock().unwrap();
            if actions.keys().any(|key| values.contains_key(key)) {
                return Err(binding_conflict());
            }
            for (key, action) in actions {
                let KVAction::Create(value) = action else {
                    panic!("unexpected overwrite in enrollment transaction");
                };
                values.insert(key, (value, 1));
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn pbl_first_hop_and_cas_share_the_existing_id_without_replacing_the_account() {
        let kv = EnrollmentKv::default();
        let zone = DID::new("bns", "enrollment-test");
        let request = NcuCasLoginRequest {
            identity_digest: ncu_cas_identity_digest("local-test", TEST_IDENTITY_KEY, "0000042").unwrap(),
            user_type: UserType::Limited,
        };
        let id = request.user_id().unwrap();
        let doc_key = format!("users/{id}/doc");
        let settings_key = format!("users/{id}/settings");
        assert!(prepare_pbl_account(&kv, &zone, &request.identity_digest, None).await.is_err());
        assert!(kv.0.lock().unwrap().is_empty());

        let (pbl, cas) = tokio::join!(
            prepare_pbl_account(&kv, &zone, &request.identity_digest, Some(UserType::Limited)),
            enroll(&kv, &zone, &request, &id),
        );
        let (opened, binding) = pbl.unwrap();
        cas.unwrap();
        assert_eq!(opened, id);
        assert_eq!(binding.user_type, UserType::Limited);
        assert_eq!(kv.0.lock().unwrap().len(), 4);
        let first_doc = kv.get(&doc_key).await.unwrap().unwrap();
        let parsed_doc: OwnerDocument = serde_json::from_str(&first_doc.0).unwrap();
        assert!(request.account_key(&parsed_doc).is_some());
        let (again, binding) = prepare_pbl_account(&kv, &zone, &request.identity_digest, Some(UserType::User))
            .await.unwrap();
        assert_eq!(again, id);
        assert_eq!(binding.user_type, UserType::Limited);
        assert_eq!(kv.get(&doc_key).await.unwrap().unwrap(), first_doc);
        assert!(prepare_pbl_account(&kv, &zone, &request.identity_digest, None).await.is_ok());

        let first_settings = kv.get(&settings_key).await.unwrap().unwrap();
        kv.0.lock().unwrap().remove(&doc_key);
        assert!(prepare_pbl_account(&kv, &zone, &request.identity_digest, Some(UserType::Limited)).await.is_err());
        assert_eq!(kv.get(&settings_key).await.unwrap().unwrap(), first_settings);
        kv.0.lock().unwrap().insert(doc_key.clone(), first_doc.clone());
        kv.0.lock().unwrap().remove(&settings_key);
        assert!(prepare_pbl_account(&kv, &zone, &request.identity_digest, Some(UserType::Limited)).await.is_err());
        assert_eq!(kv.get(&doc_key).await.unwrap().unwrap(), first_doc);

        let mut disabled: UserSettings = serde_json::from_str(&first_settings.0).unwrap();
        disabled.state = UserState::Suspended("fixture".into());
        kv.0.lock().unwrap().insert(settings_key.clone(), (serde_json::to_string(&disabled).unwrap(), 2));
        assert!(prepare_pbl_account(&kv, &zone, &request.identity_digest, Some(UserType::Limited)).await.is_err());
        kv.0.lock().unwrap().insert(settings_key, first_settings);
        let mut unrelated = parsed_doc;
        unrelated.extra_info.insert(NCU_CAS_IDENTITY_MARKER.into(), json!("a".repeat(64)));
        kv.0.lock().unwrap().insert(doc_key, (serde_json::to_string(&unrelated).unwrap(), 2));
        assert!(prepare_pbl_account(&kv, &zone, &request.identity_digest, Some(UserType::Limited)).await.is_err());
    }

    #[tokio::test]
    async fn campus_enrollment_preserves_desktop_only_namespace_and_checks_collisions() {
        let zone = DID::new("bns", "desktop-residue-test");
        let request = NcuCasLoginRequest {
            identity_digest: ncu_cas_identity_digest("local-test", TEST_IDENTITY_KEY, "0000074").unwrap(),
            user_type: UserType::Limited,
        };
        let id = request.user_id().unwrap();
        let namespace = format!("users/{id}");
        let desktop = format!("{namespace}/desktop/session/state");
        let original = ("{\"unchanged\":true}".to_owned(), 7);
        let kv = EnrollmentKv::default();
        let agent = AgentNameIndex::load(&kv, &zone).await.unwrap().check(&id).unwrap();
        kv.0.lock().unwrap().insert(desktop.clone(), original.clone());
        assert!(AgentNameIndex::load(&kv, &zone).await.unwrap().check(&id).is_err());

        for conflict in [
            namespace.clone(),
            format!("{namespace}/profile"),
            format!("{namespace}/apps/other"),
            format!("services/control_panel/agent_names/{agent}"),
            format!("security/{id}/key"),
        ] {
            kv.0.lock().unwrap().insert(conflict.clone(), ("{}".to_owned(), 3));
            assert!(enroll(&kv, &zone, &request, &id).await.is_err());
            assert_eq!(kv.0.lock().unwrap().len(), 2);
            assert_eq!(kv.get(&desktop).await.unwrap().as_ref(), Some(&original));
            kv.0.lock().unwrap().remove(&conflict);
        }
        assert!(enroll(&kv, &zone, &request, "another-user").await.is_err());
        assert!(matches!(
            enroll(&kv, &zone, &request, &id).await.unwrap(),
            AccountEnrollment::Created
        ));
        assert_eq!(kv.get(&desktop).await.unwrap().as_ref(), Some(&original));
        assert_eq!(kv.0.lock().unwrap().len(), 5);
    }

    fn callback(state: &str, cookies: &str) -> Request<Body> {
        Request::builder()
            .uri(format!(
                "{CALLBACK_PATH}?state={state}&redirect_url=https%3A%2F%2Foutside.example"
            ))
            .header(COOKIE, cookies)
            .body(ControlPanelServer::boxed_http_body(Vec::new()))
            .unwrap()
    }

    #[test]
    fn ncu_cas_return_targets_are_confined_to_the_zone() {
        let root = Url::parse("https://zone.example/sso/cas/callback").unwrap();
        for (input, expected) in [
            (
                "/workspace?tab=all%2Bnew#top",
                "/workspace?tab=all%2Bnew#top",
            ),
            ("https://zone.example/workspace#test", "/workspace#test"),
            (
                "/sso/entry?service=aiworkspace&entry=test&join=1#resume",
                "/sso/entry?service=aiworkspace&entry=test&join=1#resume",
            ),
            ("https://zone.example", "/"),
        ] {
            assert_eq!(
                ReturnTarget::parse(input, &root)
                    .unwrap()
                    .success_location(),
                expected
            );
        }
        let child_url = "https://aiws-2611a0b3.zone.example/surface?id=1#block";
        let child = ReturnTarget::parse(child_url, &root).unwrap();
        assert!(matches!(child, ReturnTarget::Workspace(_)));
        let handoff = child.success_location();
        assert!(handoff.starts_with("/login?"));
        assert_eq!(
            query_param(
                handoff.split_once('?').unwrap().1,
                "redirect_url",
                MAX_REDIRECT_URL
            )
            .as_deref(),
            Some(child_url)
        );

        for input in [
            "https://outside.example",
            "//outside.example",
            "/a/..//outside.example",
            "/%2foutside.example",
            "/%5coutside.example",
            "/%252foutside.example",
            "https://zone.example.outside.example",
            "https://user@zone.example/workspace",
            "https://@zone.example/workspace",
            "https:///zone.example/workspace",
            "http://zone.example/workspace",
            "https://zone.example:444/workspace",
            "https://aiws-x.other.zone.example/",
            "https://aiws-x-zone.example/",
            "https://aiws-.zone.example/",
            "https://aiws-x_y.zone.example/",
            "https://aiws-x.zone.example/sso/entry",
            "https://www.zone.example/",
            "javascript:alert(1)",
            "/login",
            "/LOGIN/",
            "/%6cogin",
            "/x/../login",
            "/sso/cas/start",
            "/sso/cas/callback",
            "/sso_callback",
            "/sso_refresh",
            "/sso/entry/other",
            "/sso/logout",
        ] {
            assert!(ReturnTarget::parse(input, &root).is_none(), "{input}");
        }
        assert!(
            ReturnTarget::parse(&format!("/{}", "a".repeat(MAX_REDIRECT_URL)), &root).is_none()
        );
    }

    #[test]
    fn ncu_cas_concurrent_callbacks_keep_their_own_target() {
        let now = Instant::now();
        let root = Url::parse("https://zone.example/sso/cas/callback").unwrap();
        let first_target = ReturnTarget::parse("/workspace", &root).unwrap();
        let second_target = ReturnTarget::parse("/sso/entry?entry=two#resume", &root).unwrap();
        let empty = Request::new(ControlPanelServer::boxed_http_body(Vec::new()));
        let mut pending = PendingLogins::default();
        let (first, first_browser) = pending
            .begin(now, &empty, Some(first_target.clone()))
            .unwrap();
        let (second, second_browser) = pending
            .begin(now, &empty, Some(second_target.clone()))
            .unwrap();
        assert_ne!(first, second);
        let first_cookie = format!("{BROWSER_COOKIE_PREFIX}{first}={first_browser}");
        let second_cookie = format!("{BROWSER_COOKIE_PREFIX}{second}={second_browser}");
        let both = format!("{first_cookie}; {second_cookie}");
        assert!(pending
            .consume(&callback(&first, &second_cookie), now)
            .is_none());
        assert_eq!(
            pending.consume(&callback(&first, &both), now),
            Some((first.clone(), Some(first_target)))
        );
        assert!(pending.consume(&callback(&first, &both), now).is_none());
        let invalid = failed("invalid_callback", None, None).unwrap();
        assert!(!invalid.headers().contains_key(SET_COOKIE));

        let (used, target) = pending.consume(&callback(&second, &both), now).unwrap();
        assert_eq!(target.as_ref(), Some(&second_target));
        let error = failed("provider_denied", target.as_ref(), Some(&used)).unwrap();
        let location = error.headers().get(LOCATION).unwrap().to_str().unwrap();
        let query = location.split_once('?').unwrap().1;
        assert_eq!(
            query_param(query, "cas_error", 64).as_deref(),
            Some("provider_denied")
        );
        assert_eq!(
            query_param(query, "redirect_url", MAX_REDIRECT_URL).as_deref(),
            Some(second_target.url().as_str())
        );
        assert_eq!(
            error.headers().get(SET_COOKIE).unwrap().to_str().unwrap(),
            browser_cookie(&second, None),
        );
        assert!(pending.consume(&callback(&second, &both), now).is_none());

        let (expired, expired_browser) = pending.begin(now, &empty, None).unwrap();
        let expired_cookie = format!("{BROWSER_COOKIE_PREFIX}{expired}={expired_browser}");
        assert!(pending
            .consume(&callback(&expired, &expired_cookie), now + LOGIN_TTL)
            .is_none());
        assert!(query_param("state=one&state=two", "state", 43).is_none());
    }

    #[test]
    fn ncu_cas_decodes_provider_envelopes_without_reinterpreting_denials() {
        for code in ["200", r#""200""#] {
            let body =
                format!(r#"{{"code":{code},"data":{{"userId":"staff-a","userType":"TEACHER"}}}}"#);
            let user = parse_provider_user(body.as_bytes()).unwrap();
            assert_eq!(user.user_id, "staff-a");
            assert_eq!(user.user_type, "TEACHER");
        }
        for body in [
            br#"{"code":"500","data":null}"#.as_slice(),
            br#"{"code":403,"data":{"userId":"staff-a","userType":"TEACHER"}}"#,
            br#"{"code":"403","data":"error-specific details"}"#,
            br#"{"code":403,"data":{"providerStatus":1}}"#,
            br#"{"code":200,"data":null}"#,
        ] {
            assert_eq!(parse_provider_user(body).err(), Some("provider_denied"));
        }
        for body in [
            br#"{"code":"+200","data":null}"#.as_slice(),
            br#"{"code":" 200","data":null}"#,
            br#"{"code":"0200","data":null}"#,
            br#"{"code":200.0,"data":null}"#,
            br#"{"code":true,"data":null}"#,
            br#"{"code":"200","data":{"userId":123,"userType":"TEACHER"}}"#,
        ] {
            assert_eq!(
                parse_provider_user(body).err(),
                Some("provider_unavailable")
            );
        }
    }

    #[test]
    fn ncu_cas_scopes_identities_and_rejects_unrecognized_roles() {
        let data = br#"{"code":200,"data":{"userId":"staff-a","userType":"ALUMNI"}}"#;
        assert!(parse_provider_user(data)
            .unwrap()
            .login_request("x", TEST_IDENTITY_KEY)
            .is_none());
        let data = br#"{"code":200,"data":{"userId":"staff-a","userType":"TEACHER"}}"#;
        let user = parse_provider_user(data).unwrap();
        let a = user.login_request("a", TEST_IDENTITY_KEY).unwrap();
        let b = user.login_request("b", TEST_IDENTITY_KEY).unwrap();
        assert_eq!(a.user_type, UserType::User);
        assert_ne!(a.user_id(), b.user_id());
        assert_ne!(
            a.user_id(),
            user.login_request("a", &"ab".repeat(32)).unwrap().user_id()
        );
        let data = br#"{"code":200,"data":{"userId":"student-b","userType":"GRADUATE"}}"#;
        let user = parse_provider_user(data).unwrap();
        assert_eq!(
            user.login_request("a", TEST_IDENTITY_KEY)
                .unwrap()
                .user_type,
            UserType::Limited
        );
    }

    #[test]
    fn ncu_cas_builds_fixed_provider_authorize_and_signs_in_guide_order() {
        let config = Config {
            app_key: "demo-key".into(),
            app_secret: "demo-secret".into(),
            identity_key: TEST_IDENTITY_KEY.into(),
            callback_url: "https://test.example/sso/cas/callback".parse().unwrap(),
        };
        let url = config.authorize_url("browser-state");
        assert_eq!(url.host_str(), Some("iaa.ncu.edu.cn"));
        let query: HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(query["mode"], "cas");
        assert_eq!(query["state"], "browser-state");
        assert_eq!(query["redirectUri"], config.callback_url.as_str());
        assert_eq!(query.len(), 4);
        assert_eq!(query["clientId"], "demo-key");
        assert_eq!(
            config.signature("1700000000000", "1234").as_deref(),
            Some("7D7DC6F3B34AD21683F788F79B60304F7631B40E1C8C61053974F901700A2A08")
        );
    }
}
