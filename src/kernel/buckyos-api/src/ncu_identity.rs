use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use jsonwebtoken::{crypto, Algorithm, EncodingKey};
use serde::Deserialize;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use url::Url;

pub const NCU_CAS_CONFIG_FILE: &str = "ncu-cas-test.json";
pub const NCU_CAS_CALLBACK_PATH: &str = "/sso/cas/callback";
pub const NCU_PBL_TEACHER_KEY_FILE: &str = "ncu-pbl-teacher.key";
const NCU_IAADP_AUTHORIZE: &str = "https://iaa.ncu.edu.cn/openapi/v1/auth/sso/authorize";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NcuCasConfig {
    pub app_key: String,
    pub app_secret: String,
    pub identity_key: String,
    pub callback_url: Url,
}

impl NcuCasConfig {
    pub fn read(file: std::fs::File) -> Option<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            let metadata = file.metadata().ok()?;
            if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o077 != 0 {
                return None;
            }
        }
        let config: Self = serde_json::from_reader(file.take(8192)).ok()?;
        let url = &config.callback_url;
        (url.scheme() == "https"
            && url.domain().is_some()
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == NCU_CAS_CALLBACK_PATH
            && url.query().is_none()
            && url.fragment().is_none()
            && !config.app_key.is_empty()
            && config.app_key.len() <= 128
            && config
                .app_key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
            && !config.app_secret.is_empty()
            && config.app_secret.len() <= 1024
            && config.identity_key.len() == 64
            && config
                .identity_key
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        .then_some(config)
    }

    pub fn authorize_url(&self, state: &str) -> Url {
        let mut url = Url::parse(NCU_IAADP_AUTHORIZE).expect("fixed NCU authorize URL");
        url.query_pairs_mut()
            .append_pair("clientId", &self.app_key)
            .append_pair("redirectUri", self.callback_url.as_str())
            .append_pair("mode", "cas")
            .append_pair("state", state);
        url
    }

    pub fn signature(&self, timestamp: &str, nonce: &str) -> Option<String> {
        let canonical = format!(
            "X-Open-AppKey={}&X-Open-Nonce={nonce}&X-Open-Timestamp={timestamp}&app_secret={}",
            self.app_key, self.app_secret
        );
        Some(hex::encode_upper(hmac_sha256(
            self.app_secret.as_bytes(),
            canonical.as_bytes(),
        )?))
    }
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> Option<Vec<u8>> {
    let signed = crypto::sign(message, &EncodingKey::from_secret(key), Algorithm::HS256).ok()?;
    URL_SAFE_NO_PAD.decode(signed).ok()
}

/// The exact existing CAS binding for a provider identity, shared by both bridges.
pub fn ncu_cas_identity_digest(
    app_key: &str,
    identity_key: &str,
    school_id: &str,
) -> Option<String> {
    if school_id.is_empty()
        || school_id.len() > 256
        || school_id
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        return None;
    }
    let subject = format!("iaa.ncu.edu.cn\0{app_key}\0{school_id}");
    let key = hex::decode(identity_key).ok()?;
    Some(hex::encode(hmac_sha256(&key, subject.as_bytes())?))
}

/// Account classification from the campus provider. PBL's active role is independent.
pub fn ncu_school_user_type(provider_type: &str) -> Option<crate::UserType> {
    match provider_type {
        "TEACHER" => Some(crate::UserType::User),
        "UNDERGRADUATE" | "GRADUATE" => Some(crate::UserType::Limited),
        _ => None,
    }
}

pub fn is_ncu_handoff_token(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

#[cfg(unix)]
pub fn load_ncu_pbl_teacher_key(etc_dir: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;

    let path = etc_dir.join(NCU_PBL_TEACHER_KEY_FILE);
    if !std::fs::symlink_metadata(&path).ok()?.is_file() {
        return None;
    }
    let file = File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o077 != 0 {
        return None;
    }
    let mut text = String::new();
    file.take(128).read_to_string(&mut text).ok()?;
    let key = text.trim_end_matches(['\r', '\n']);
    is_ncu_handoff_token(key).then(|| key.to_owned())
}

#[cfg(not(unix))]
pub fn load_ncu_pbl_teacher_key(_etc_dir: &Path) -> Option<String> {
    None
}
