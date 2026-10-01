//! Cookie rotation and `__Secure-1PSIDTS` refresh logic for Gemini Web sessions.

use http::HeaderMap;
use reqwest::Method;
use url::Url;

use gemini_bridge_transport::{Idempotency, ReqwestTransport, TransportRequest, TransportService};

use crate::error::IdentityError;
use crate::model::{SessionBootstrap, SessionCredentials};
use crate::parser;

/// Result of refreshing the session through an authenticated `/app` request.
pub struct RotationResult {
    /// Fresh bootstrap tokens from the `/app` response.
    pub bootstrap: SessionBootstrap,
    /// The rotated `__Secure-1PSIDTS` cookie value.
    pub psidts: String,
}

/// Fetch current bootstrap values and the renewed `__Secure-1PSIDTS` cookie.
///
/// The response must carry a non-empty rotated cookie; otherwise this operation
/// fails closed instead of reporting a refresh that did not actually happen.
pub async fn execute_rotation(
    transport: &ReqwestTransport,
    base_url: Option<&str>,
    credentials: &SessionCredentials,
) -> Result<RotationResult, IdentityError> {
    let base = base_url.unwrap_or("https://gemini.google.com");
    let url = Url::parse(&format!("{base}/app")).map_err(|_| IdentityError::Transport)?;

    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::COOKIE,
        format!(
            "__Secure-1PSID={}; __Secure-1PSIDTS={}; SAPISID={}",
            credentials.psid, credentials.psidts, credentials.sapisid
        )
        .parse()
        .map_err(|_| IdentityError::Transport)?,
    );
    headers.insert(
        http::header::AUTHORIZATION,
        parser::build_sapisidhash(&credentials.sapisid)
            .parse()
            .map_err(|_| IdentityError::Transport)?,
    );
    headers.insert(
        http::header::USER_AGENT,
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/120.0.0.0 Safari/537.36"
            .parse()
            .map_err(|_| IdentityError::Transport)?,
    );

    let response = transport
        .execute(TransportRequest {
            method: Method::GET,
            url,
            headers,
            body: None,
            idempotency: Idempotency::SafeToRetry,
        })
        .await
        .map_err(|_| IdentityError::Transport)?;

    if response.status.as_u16() == 301 || response.status.as_u16() == 302 {
        let location = response
            .headers
            .get(http::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        if location.contains("sorry") {
            return Err(IdentityError::IpFlagged);
        }
    }
    if !response.status.is_success() {
        return Err(IdentityError::NeedsReauth);
    }

    let psidts = response
        .headers
        .get_all(http::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(parse_psidts_set_cookie)
        .ok_or(IdentityError::NeedsReauth)?;

    let html = String::from_utf8_lossy(&response.body);
    let bootstrap = SessionBootstrap {
        bl: parser::extract_bl(&html).ok_or(IdentityError::MissingBootstrapField("bl"))?,
        snlm0e: parser::extract_snlm0e(&html)
            .ok_or(IdentityError::MissingBootstrapField("SNlM0e"))?,
        fsid: parser::extract_fsid(&html).ok_or(IdentityError::MissingBootstrapField("f.sid"))?,
    };

    Ok(RotationResult { bootstrap, psidts })
}

/// Extract the value of a `__Secure-1PSIDTS` pair from one `Set-Cookie` header.
fn parse_psidts_set_cookie(header: &str) -> Option<String> {
    let pair = header.split(';').next()?.trim();
    let (name, value) = pair.split_once('=')?;
    (name == "__Secure-1PSIDTS" && !value.is_empty()).then(|| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::parse_psidts_set_cookie;

    #[test]
    fn extracts_only_non_empty_1psidts_cookie_pair() {
        assert_eq!(
            parse_psidts_set_cookie("__Secure-1PSIDTS=rotated; Path=/; Secure").as_deref(),
            Some("rotated")
        );
        assert_eq!(parse_psidts_set_cookie("SAPISID=other; Path=/"), None);
        assert_eq!(parse_psidts_set_cookie("__Secure-1PSIDTS=; Path=/"), None);
    }
}
