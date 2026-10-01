use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use async_trait::async_trait;
use bytes::Bytes;
use gemini_bridge_media_store::LocalMediaStore;
use gemini_bridge_upload::{
    PushUploadClient, UploadError, UploadLimits, UploadService, UploadServiceImpl,
    is_address_allowed, validate_reference_url,
};
use tempfile::TempDir;
use url::Url;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nimage-bytes";

#[derive(Default)]
struct RecordingPushClient {
    initiate_calls: tokio::sync::Mutex<usize>,
    upload_calls: tokio::sync::Mutex<usize>,
}

#[async_trait]
impl PushUploadClient for RecordingPushClient {
    async fn initiate_session(
        &self,
        mime_type: &str,
        size_bytes: u64,
    ) -> Result<String, UploadError> {
        assert_eq!(mime_type, "image/png");
        assert_eq!(size_bytes, PNG.len() as u64);
        *self.initiate_calls.lock().await += 1;
        Ok("https://upload.example/session-secret".into())
    }

    async fn upload_bytes(&self, session_url: &str, bytes: Bytes) -> Result<String, UploadError> {
        assert_eq!(session_url, "https://upload.example/session-secret");
        assert_eq!(bytes, Bytes::from_static(PNG));
        *self.upload_calls.lock().await += 1;
        Ok("files/reference-1".into())
    }
}

#[tokio::test]
async fn upload_validates_magic_bytes_stores_content_and_deduplicates_file_ref() {
    let temp = TempDir::new().unwrap();
    let push = std::sync::Arc::new(RecordingPushClient::default());
    let service = UploadServiceImpl::new(
        LocalMediaStore::new(temp.path()),
        push.clone(),
        UploadLimits::default(),
    );

    let first = service
        .upload_bytes(Bytes::from_static(PNG), Some("application/octet-stream"))
        .await
        .unwrap();
    let second = service
        .upload_bytes(Bytes::from_static(PNG), Some("image/jpeg"))
        .await
        .unwrap();

    assert_eq!(first.id, second.id);
    assert_eq!(first.file_ref, "files/reference-1");
    assert_eq!(first, second);
    assert_eq!(
        service.get(&first.id).await.unwrap(),
        Bytes::from_static(PNG)
    );
    assert_eq!(*push.initiate_calls.lock().await, 1);
    assert_eq!(*push.upload_calls.lock().await, 1);
}

#[tokio::test]
async fn upload_rejects_claimed_image_with_invalid_bytes_and_enforces_limit() {
    let temp = TempDir::new().unwrap();
    let push = std::sync::Arc::new(RecordingPushClient::default());
    let service = UploadServiceImpl::new(
        LocalMediaStore::new(temp.path()),
        push,
        UploadLimits {
            max_bytes: PNG.len() as u64 - 1,
            ..UploadLimits::default()
        },
    );

    assert!(matches!(
        service
            .upload_bytes(Bytes::from_static(PNG), Some("image/png"))
            .await,
        Err(UploadError::TooLarge { .. })
    ));

    let service = UploadServiceImpl::new(
        LocalMediaStore::new(temp.path()),
        std::sync::Arc::new(RecordingPushClient::default()),
        UploadLimits::default(),
    );
    assert!(matches!(
        service
            .upload_bytes(Bytes::from_static(b"not an image"), Some("image/png"))
            .await,
        Err(UploadError::UnsupportedType(_))
    ));
}

#[test]
fn ssrf_policy_blocks_private_loopback_link_local_and_special_addresses() {
    let blocked = [
        IpAddr::V4(Ipv4Addr::new(0, 0, 0, 1)),
        IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3)),
        IpAddr::V4(Ipv4Addr::new(100, 64, 1, 1)),
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
        IpAddr::V4(Ipv4Addr::new(169, 254, 1, 1)),
        IpAddr::V4(Ipv4Addr::new(172, 16, 1, 1)),
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
        IpAddr::V4(Ipv4Addr::BROADCAST),
        IpAddr::V6(Ipv6Addr::LOCALHOST),
        IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        "fc00::1".parse().unwrap(),
        "fe80::1".parse().unwrap(),
        "::ffff:10.0.0.1".parse().unwrap(),
    ];

    for address in blocked {
        assert!(!is_address_allowed(address), "{address} must be blocked");
    }
    assert!(is_address_allowed("8.8.8.8".parse().unwrap()));
    assert!(is_address_allowed("2606:4700:4700::1111".parse().unwrap()));
}

#[test]
fn ssrf_policy_rejects_forbidden_schemes_and_literal_private_hosts() {
    for url in [
        "file:///etc/passwd",
        "data:image/png;base64,AA==",
        "ftp://example.com/image.png",
        "http://127.0.0.1/image.png",
        "http://[::1]/image.png",
    ] {
        let parsed = Url::parse(url).unwrap();
        assert!(matches!(
            validate_reference_url(&parsed),
            Err(UploadError::SsrfDenied(_))
        ));
    }
}
