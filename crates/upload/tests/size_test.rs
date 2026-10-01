use async_trait::async_trait;
use bytes::Bytes;
use gemini_bridge_media_store::LocalMediaStore;
use gemini_bridge_upload::push_client::PushUploadClient;
use gemini_bridge_upload::{UploadError, UploadLimits, UploadService, UploadServiceImpl};
use tempfile::TempDir;

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

struct MockPushClient;

#[async_trait]
impl PushUploadClient for MockPushClient {
    async fn initiate_session(
        &self,
        _mime_type: &str,
        _size_bytes: u64,
    ) -> Result<String, UploadError> {
        Ok("https://upload.example/session".into())
    }

    async fn upload_bytes(&self, _session_url: &str, _bytes: Bytes) -> Result<String, UploadError> {
        Ok("files/ref".into())
    }
}

#[tokio::test]
async fn body_exactly_at_limit_is_accepted() {
    let temp = TempDir::new().unwrap();
    let size = 100u64;
    let mut payload = PNG_SIGNATURE.to_vec();
    payload.resize(size as usize, 0);

    let service = UploadServiceImpl::new(
        LocalMediaStore::new(temp.path()),
        std::sync::Arc::new(MockPushClient),
        UploadLimits {
            max_bytes: size,
            ..UploadLimits::default()
        },
    );

    let result = service.upload_bytes(Bytes::from(payload), None).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn body_one_byte_over_limit_is_rejected() {
    let temp = TempDir::new().unwrap();
    let size = 100u64;
    let mut payload = PNG_SIGNATURE.to_vec();
    payload.resize((size + 1) as usize, 0);

    let service = UploadServiceImpl::new(
        LocalMediaStore::new(temp.path()),
        std::sync::Arc::new(MockPushClient),
        UploadLimits {
            max_bytes: size,
            ..UploadLimits::default()
        },
    );

    let result = service.upload_bytes(Bytes::from(payload), None).await;
    assert!(matches!(
        result,
        Err(UploadError::TooLarge {
            limit: 100,
            received: 101
        })
    ));
}
