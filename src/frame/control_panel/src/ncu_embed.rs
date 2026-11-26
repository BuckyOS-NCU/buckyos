//! NCU fork: the cross-site embedding switch for the SSO cookies.
//!
//! The Desktop is to be embedded in an iframe of another site (the school's
//! project teaching system). A browser drops `SameSite=Lax` cookies in such a
//! frame, so the session cannot survive there. With the switch on, the cookies
//! `/sso_callback`, `/sso_refresh` and `/sso_logout` write become
//! `SameSite=None; Secure; Partitioned`: `Secure` is required for
//! `SameSite=None` whatever `X-Forwarded-Proto` says, and `Partitioned` keys
//! them to the embedding top-level site (CHIPS).
//!
//! The switch is `<etc>/ncu_embed.json` holding `{"cross_site_cookie": true}`.
//! A missing file, any other content or an unreadable file means off: the
//! cookies stay exactly as upstream writes them. The file is read each time a
//! cookie is written, so turning the switch on or off needs no restart.

use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};

pub(crate) const CONFIG_FILE_NAME: &str = "ncu_embed.json";

/// The cookie attributes the switch decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CookieSitePolicy {
    /// The `SameSite` value.
    pub same_site: &'static str,
    /// `Secure` even when the request does not say it arrived over HTTPS.
    pub force_secure: bool,
    /// `Partitioned` (CHIPS).
    pub partitioned: bool,
}

impl CookieSitePolicy {
    /// What upstream writes: `SameSite=Lax`, `Secure` only behind HTTPS.
    pub(crate) const UPSTREAM: Self = Self {
        same_site: "Lax",
        force_secure: false,
        partitioned: false,
    };
    /// Switch on: `SameSite=None; Secure; Partitioned`.
    pub(crate) const CROSS_SITE: Self = Self {
        same_site: "None",
        force_secure: true,
        partitioned: true,
    };
}

/// The policy for the cookie being written now.
pub(crate) fn cookie_site_policy() -> CookieSitePolicy {
    let enabled = cross_site_cookie_enabled();
    log_when_changed(enabled);
    if enabled {
        CookieSitePolicy::CROSS_SITE
    } else {
        CookieSitePolicy::UPSTREAM
    }
}

#[cfg(not(test))]
fn cross_site_cookie_enabled() -> bool {
    read_switch(&crate::gateway_etc_dir().join(CONFIG_FILE_NAME))
}

/// Unit tests never read the machine's deployment, so the upstream cookie
/// tests keep asserting the upstream attributes wherever they run.
#[cfg(test)]
fn cross_site_cookie_enabled() -> bool {
    false
}

fn read_switch(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .map(|raw| parse_switch(&raw))
        .unwrap_or(false)
}

fn parse_switch(raw: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|value| {
            value
                .get("cross_site_cookie")
                .and_then(serde_json::Value::as_bool)
        })
        .unwrap_or(false)
}

/// 0 = not logged yet, 1 = off, 2 = on.
static LAST_LOGGED: AtomicU8 = AtomicU8::new(0);

fn log_when_changed(enabled: bool) {
    let state = if enabled { 2 } else { 1 };
    if LAST_LOGGED.swap(state, Ordering::Relaxed) != state {
        if enabled {
            log::info!("ncu_embed: cross-site SSO cookies on (SameSite=None; Secure; Partitioned)");
        } else {
            log::info!("ncu_embed: cross-site SSO cookies off (upstream SameSite=Lax)");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_is_on_only_for_an_explicit_true() {
        assert!(parse_switch(r#"{"cross_site_cookie": true}"#));
        assert!(parse_switch(r#"{"cross_site_cookie": true, "note": "M1"}"#));
        assert!(!parse_switch(r#"{"cross_site_cookie": false}"#));
        assert!(!parse_switch(r#"{"cross_site_cookie": "true"}"#));
        assert!(!parse_switch(r#"{"cross_site_cookie": 1}"#));
        assert!(!parse_switch(r#"{}"#));
        assert!(!parse_switch(""));
        assert!(!parse_switch("not json"));
    }

    #[test]
    fn missing_or_present_switch_file() {
        let dir = std::env::temp_dir().join(format!("ncu_embed_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(CONFIG_FILE_NAME);
        let _ = std::fs::remove_file(&path);
        assert!(!read_switch(&path));
        std::fs::write(&path, r#"{"cross_site_cookie": true}"#).unwrap();
        assert!(read_switch(&path));
        std::fs::write(&path, r#"{"cross_site_cookie": false}"#).unwrap();
        assert!(!read_switch(&path));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn plain_http_request() -> http::Request<
        http_body_util::combinators::BoxBody<bytes::Bytes, buckyos_http_server::ServerError>,
    > {
        // No X-Forwarded-Proto: upstream would leave `Secure` out.
        http::Request::builder()
            .uri("/sso_refresh")
            .header(http::header::HOST, "files.example.test")
            .body(crate::ControlPanelServer::boxed_http_body(Vec::new()))
            .unwrap()
    }

    #[test]
    fn cross_site_cookie_is_none_secure_partitioned_even_without_https_header() {
        let req = plain_http_request();
        let set = crate::ControlPanelServer::build_host_cookie_header_with(
            &req,
            "buckyos_refresh_token",
            Some("t"),
            true,
            Some(300),
            CookieSitePolicy::CROSS_SITE,
        );
        assert_eq!(
            set,
            "buckyos_refresh_token=t; Path=/; SameSite=None; Max-Age=300; HttpOnly; Secure; Partitioned"
        );
        let clear = crate::ControlPanelServer::build_host_cookie_header_with(
            &req,
            "buckyos_session_token",
            None,
            false,
            None,
            CookieSitePolicy::CROSS_SITE,
        );
        assert_eq!(
            clear,
            "buckyos_session_token=; Path=/; SameSite=None; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Secure; Partitioned"
        );
    }

    #[test]
    fn upstream_policy_keeps_the_upstream_header() {
        let req = plain_http_request();
        let set = crate::ControlPanelServer::build_host_cookie_header_with(
            &req,
            "buckyos_refresh_token",
            Some("t"),
            true,
            Some(300),
            CookieSitePolicy::UPSTREAM,
        );
        assert_eq!(
            set,
            "buckyos_refresh_token=t; Path=/; SameSite=Lax; Max-Age=300; HttpOnly"
        );
    }

    #[test]
    fn policies_match_the_attributes_they_name() {
        assert_eq!(CookieSitePolicy::UPSTREAM.same_site, "Lax");
        assert!(!CookieSitePolicy::UPSTREAM.force_secure);
        assert!(!CookieSitePolicy::UPSTREAM.partitioned);
        assert_eq!(CookieSitePolicy::CROSS_SITE.same_site, "None");
        assert!(CookieSitePolicy::CROSS_SITE.force_secure);
        assert!(CookieSitePolicy::CROSS_SITE.partitioned);
        // Unit tests run with the switch off.
        assert_eq!(cookie_site_policy(), CookieSitePolicy::UPSTREAM);
    }
}
