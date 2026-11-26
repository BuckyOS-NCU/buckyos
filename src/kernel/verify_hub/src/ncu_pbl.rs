use ::kRPC::RPCErrors;
use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use buckyos_api::{
    is_ncu_handoff_token, load_ncu_pbl_teacher_key, ncu_school_user_type, NcuCasConfig, NcuPblTeacherLoginRequest,
    NcuPblTeacherEnrollment, UserType, NCU_CAS_CONFIG_FILE,
};
use buckyos_kit::get_buckyos_system_etc_dir;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const REDEEM_URL: &str = "https://139.196.234.199/xmz/api/buckyos/handoff/redeem";
const CHECK_URL: &str = "https://139.196.234.199/xmz/api/buckyos/handoff/check";
const MAX_BODY: usize = 4096;
const MAX_PREPARED: usize = 256;
const PREPARED_TTL_SECONDS: u64 = 60;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Receipt {
    pub school_id: String,
    pub grant: String,
    pub expires_at: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EnrollmentReceipt {
    pub school_id: String,
    pub grant: String,
    pub expires_at: u64,
    pub school_user_type: Option<String>,
}

impl EnrollmentReceipt {
    fn new_account_type(&self) -> Option<UserType> {
        self.school_user_type.as_deref().and_then(ncu_school_user_type)
    }
}

pub(super) struct PreparedEnrollment {
    pub identity_digest: String,
    pub receipt: EnrollmentReceipt,
    deadline: u64,
}

#[derive(Default)]
pub(super) struct PendingEnrollments(Mutex<HashMap<String, PreparedEnrollment>>);

impl PendingEnrollments {
    pub fn prepare(
        &self,
        identity_digest: String,
        receipt: EnrollmentReceipt,
        now: u64,
    ) -> Option<NcuPblTeacherEnrollment> {
        let deadline = receipt.expires_at.min(now + PREPARED_TTL_SECONDS);
        if deadline <= now {
            return None;
        }
        let mut pending = self.0.lock().ok()?;
        pending.retain(|_, item| item.deadline > now);
        if pending.len() >= MAX_PREPARED {
            return None;
        }
        let enrollment_id = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
        let public = NcuPblTeacherEnrollment {
            enrollment_id: enrollment_id.clone(),
            identity_digest: identity_digest.clone(),
            new_account_type: receipt.new_account_type(),
        };
        if let std::collections::hash_map::Entry::Vacant(entry) = pending.entry(enrollment_id) {
            entry.insert(PreparedEnrollment { identity_digest, receipt, deadline });
            Some(public)
        } else {
            None
        }
    }

    pub fn take(&self, enrollment_id: &str, now: u64) -> Option<PreparedEnrollment> {
        if !is_ncu_handoff_token(enrollment_id) {
            return None;
        }
        self.0.lock().ok()?.remove(enrollment_id).filter(|item| item.deadline > now)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantStatus {
    active: bool,
}

pub(super) fn load_cas_config() -> Option<NcuCasConfig> {
    let file = std::fs::File::open(get_buckyos_system_etc_dir().join(NCU_CAS_CONFIG_FILE)).ok()?;
    NcuCasConfig::read(file)
}

fn client() -> Option<&'static reqwest::Client> {
    static HTTP: OnceLock<Option<reqwest::Client>> = OnceLock::new();
    HTTP.get_or_init(|| {
        reqwest::Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()
            .ok()
    })
    .as_ref()
}

async fn exchange<T: DeserializeOwned>(url: &'static str, payload: Value) -> Option<T> {
    let key = load_ncu_pbl_teacher_key(&get_buckyos_system_etc_dir())?;
    let mut response = client()?
        .post(url)
        .bearer_auth(key)
        .json(&payload)
        .send()
        .await
        .ok()?;
    if response.status() != reqwest::StatusCode::OK {
        return None;
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if body.len() + chunk.len() > MAX_BODY {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).ok()
}

pub(super) async fn redeem(request: NcuPblTeacherLoginRequest) -> Result<Receipt, RPCErrors> {
    if !is_ncu_handoff_token(&request.state) || !is_ncu_handoff_token(&request.code) {
        return Err(RPCErrors::ParseRequestError("invalid PBL handoff".into()));
    }
    exchange::<Receipt>(
        REDEEM_URL,
        json!({"state": request.state, "code": request.code}),
    )
    .await
    .filter(|receipt| is_ncu_handoff_token(&receipt.grant))
    .ok_or_else(|| RPCErrors::NoPermission("PBL handoff refused".into()))
}

pub(super) async fn redeem_enrollment(request: NcuPblTeacherLoginRequest) -> Result<EnrollmentReceipt, RPCErrors> {
    if !is_ncu_handoff_token(&request.state) || !is_ncu_handoff_token(&request.code) {
        return Err(RPCErrors::ParseRequestError("invalid PBL handoff".into()));
    }
    exchange::<EnrollmentReceipt>(
        REDEEM_URL,
        json!({"state": request.state, "code": request.code, "schema_version": 2}),
    )
    .await
    .filter(|receipt| is_ncu_handoff_token(&receipt.grant))
    .ok_or_else(|| RPCErrors::NoPermission("PBL handoff refused".into()))
}

#[async_trait]
pub(super) trait GrantStatusChecker: Send + Sync {
    async fn grant_active(&self, grant: &str) -> bool;
    async fn enrollment_grant_active(&self, grant: &str, school_id: &str) -> bool;
}

pub(super) struct Backchannel;

#[async_trait]
impl GrantStatusChecker for Backchannel {
    async fn grant_active(&self, grant: &str) -> bool {
        is_ncu_handoff_token(grant)
            && exchange::<GrantStatus>(CHECK_URL, json!({"grant": grant}))
                .await
                .is_some_and(|status| status.active)
    }
    async fn enrollment_grant_active(&self, grant: &str, school_id: &str) -> bool {
        is_ncu_handoff_token(grant)
            && exchange::<GrantStatus>(CHECK_URL, json!({"grant": grant, "school_id": school_id}))
                .await
                .is_some_and(|status| status.active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enrollment_uses_exact_optional_school_type_and_expiring_one_use_handle() {
        let receipt = |expires_at, kind: Option<&str>| EnrollmentReceipt {
            school_id: "0000042".into(),
            grant: "g".repeat(43),
            expires_at,
            school_user_type: kind.map(str::to_owned),
        };
        let pending = PendingEnrollments::default();
        let digest = "ab".repeat(32);
        assert!(pending.prepare(digest.clone(), receipt(50, None), 50).is_none());
        let old = pending.prepare(digest.clone(), receipt(150, None), 50).unwrap();
        assert!(old.new_account_type.is_none());
        assert!(pending.take(&old.enrollment_id, 110).is_none());
        let allowed = pending.prepare(digest.clone(), receipt(151, Some("TEACHER")), 100).unwrap();
        assert_eq!(allowed.new_account_type, Some(UserType::User));
        assert!(pending.take(&"w".repeat(43), 101).is_none());
        let private = pending.take(&allowed.enrollment_id, 101).unwrap();
        assert_eq!(private.identity_digest, digest);
        assert_eq!(private.receipt.school_id, "0000042");
        assert!(pending.take(&allowed.enrollment_id, 102).is_none());
        assert!(serde_json::from_value::<EnrollmentReceipt>(json!({
            "school_id":"0000042","grant":"g".repeat(43),"expires_at":151,
            "school_user_type":null,"school_display_name":"unexpected"
        })).is_err());
    }
}
