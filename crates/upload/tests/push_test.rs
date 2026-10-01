use async_trait::async_trait;
use bytes::Bytes;
use gemini_bridge_identity::{
    IdentityError, IdentityService, SessionBootstrap, SessionSnapshot, SessionStatus,
};
use gemini_bridge_transport::ReqwestTransport;
use gemini_bridge_upload::push_client::{HttpPushUploadClient, PushUploadClient};
use http::HeaderMap;
use serde_json::json;
use std::sync::Arc;
use wiremock::matchers::{header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct DummyIdentity;

#[async_trait]
impl IdentityService for DummyIdentity {
    async fn bootstrap(&self) -> Result<SessionBootstrap, IdentityError> {
        unimplemented!()
    }

    async fn refresh_1psidts(&self) -> Result<(), IdentityError> {
        Ok(())
    }

    async fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            status: SessionStatus::Valid,
            build_label: None,
            cookie_age: None,
            checked_at: std::time::SystemTime::now(),
        }
    }

    fn apply_auth_headers(&self, headers: &mut HeaderMap) -> Result<(), IdentityError> {
        headers.insert("cookie", "__Secure-1PSID=fake".parse().unwrap());
        Ok(())
    }

    async fn import_credentials(&self, _raw_cookie_header: &str) -> Result<(), IdentityError> {
        Ok(())
    }
}

fn transport() -> Arc<ReqwestTransport> {
    Arc::new(
        ReqwestTransport::new(&gemini_bridge_config::TransportConfig {
            tls_profile: "chrome".into(),
            proxy_url: None,
            timeout_secs: 10,
        })
        .unwrap(),
    )
}

#[tokio::test]
async fn push_upload_two_step_flow_succeeds() {
    let server = MockServer::start().await;
    let session_url = format!("{}/upload-session/123", server.uri());

    Mock::given(method("POST"))
        .and(path("/upload/"))
        .and(header("X-Goog-Upload-Command", "start"))
        .and(header("X-Goog-Upload-Protocol", "resumable"))
        .and(header("X-Goog-Upload-Header-Content-Length", "10"))
        .and(header("X-Goog-Upload-Header-Content-Type", "image/png"))
        .and(header_exists("cookie"))
        .respond_with(
            ResponseTemplate::new(200).insert_header("X-Goog-Upload-URL", session_url.as_str()),
        )
        .expect(1)
        .mount(&server)
        .await;

    let response_body = json!({
        "sessionStatus": {
            "additionalInfo": {
                "uploader_service.GoogleRupioAdditionalInfo": {
                    "completionInfo": {
                        "customerSpecificInfo": {
                            "fileRef": "files/test-file-ref-12345"
                        }
                    }
                }
            }
        }
    });
    Mock::given(method("POST"))
        .and(path("/upload-session/123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
        .expect(1)
        .mount(&server)
        .await;

    let client = HttpPushUploadClient::with_endpoint(
        Arc::new(DummyIdentity),
        transport(),
        format!("{}/upload/", server.uri()),
    )
    .unwrap();

    let session = client.initiate_session("image/png", 10).await.unwrap();
    assert_eq!(session, session_url);
    let file_ref = client
        .upload_bytes(&session, Bytes::from_static(b"0123456789"))
        .await
        .unwrap();
    assert_eq!(file_ref, "files/test-file-ref-12345");
}

#[tokio::test]
async fn push_upload_initiate_surfaces_upstream_failure() {
    // Transient transport retries are the shared transport layer's job: this
    // call marks initiation SafeToRetry. A non-transient HTTP status surfaces.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/upload/"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;

    let client = HttpPushUploadClient::with_endpoint(
        Arc::new(DummyIdentity),
        transport(),
        format!("{}/upload/", server.uri()),
    )
    .unwrap();

    let result = client.initiate_session("image/png", 10).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn push_upload_step2_failure_is_not_retried() {
    let server = MockServer::start().await;
    let session_url = format!("{}/upload-session/789", server.uri());
    Mock::given(method("POST"))
        .and(path("/upload-session/789"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;

    let client = HttpPushUploadClient::with_endpoint(
        Arc::new(DummyIdentity),
        transport(),
        format!("{}/upload/", server.uri()),
    )
    .unwrap();
    let result = client
        .upload_bytes(&session_url, Bytes::from_static(b"0123456789"))
        .await;
    assert!(result.is_err());
}
