use gemini_bridge_upload::{UploadError, validate_redirect_target};
use url::Url;

#[test]
fn redirect_to_private_ip_is_denied() {
    let base = Url::parse("https://example.com/image.png").unwrap();
    let result = validate_redirect_target(&base, "http://169.254.169.254/latest/meta-data");
    assert!(matches!(result, Err(UploadError::SsrfDenied(_))));
}

#[test]
fn redirect_to_non_http_scheme_is_denied() {
    let base = Url::parse("https://example.com/image.png").unwrap();
    let result = validate_redirect_target(&base, "file:///etc/passwd");
    assert!(matches!(result, Err(UploadError::SsrfDenied(_))));
}

#[test]
fn relative_public_redirect_is_allowed_for_resolution() {
    let base = Url::parse("https://example.com/path/image.png").unwrap();
    let target = validate_redirect_target(&base, "/other/image.png").unwrap();
    assert_eq!(target.as_str(), "https://example.com/other/image.png");
}
