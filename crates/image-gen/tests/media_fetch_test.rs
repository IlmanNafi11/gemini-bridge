use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use bytes::Bytes;
use futures::{Stream, stream};
use gemini_bridge_image_gen::{
    DefaultImageGenService, ImageGenError, ImageGenService, ImageGenerationRequest,
};
use gemini_bridge_llm_service::{
    Completion, LlmAdapter, LlmError, LlmEvent, LlmEventStream, LlmRequest,
};
use gemini_bridge_media_store::LocalMediaStore;
use gemini_bridge_upload::{MediaDownloader, MediaKind, UploadError, UploadService, UploadedFile};
use tempfile::TempDir;
use url::Url;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nvalidated image bytes";

struct PayloadAdapter;

#[async_trait]
impl LlmAdapter for PayloadAdapter {
    fn provider_id(&self) -> &'static str {
        "test"
    }

    async fn complete(&self, _request: Arc<LlmRequest>) -> Result<Completion, LlmError> {
        unreachable!()
    }

    async fn complete_raw(&self, _request: Arc<LlmRequest>) -> Result<serde_json::Value, LlmError> {
        Ok(serde_json::json!([
            "https://lh3.googleusercontent.com/one.png",
            "https://lh3.googleusercontent.com/two.png"
        ]))
    }

    async fn stream(&self, _request: Arc<LlmRequest>) -> Result<LlmEventStream, LlmError> {
        let empty: Pin<Box<dyn Stream<Item = Result<LlmEvent, LlmError>> + Send>> =
            Box::pin(stream::empty());
        Ok(empty)
    }
}

struct NoopUpload;

#[async_trait]
impl UploadService for NoopUpload {
    async fn upload_bytes(
        &self,
        _bytes: Bytes,
        _mime: Option<&str>,
    ) -> Result<UploadedFile, UploadError> {
        unreachable!()
    }
    async fn upload_from_url(&self, _url: Url) -> Result<UploadedFile, UploadError> {
        unreachable!()
    }
    async fn get(&self, _id: &str) -> Result<Bytes, UploadError> {
        unreachable!()
    }
    async fn get_by_hash(&self, _sha: &str) -> Option<UploadedFile> {
        None
    }
    fn max_bytes(&self) -> u64 {
        1024
    }
    async fn get_by_id_mime(&self, _id: &str) -> Option<String> {
        None
    }
    async fn get_by_id(&self, _id: &str) -> Option<UploadedFile> {
        None
    }
}

struct FixedDownloader;

#[async_trait]
impl MediaDownloader for FixedDownloader {
    async fn download(&self, url: &Url, kind: MediaKind) -> Result<Bytes, UploadError> {
        assert_eq!(kind, MediaKind::Image);
        assert!(url.host_str().unwrap().ends_with(".googleusercontent.com"));
        Ok(Bytes::from_static(PNG))
    }
}

fn request(n: u32, format: &str) -> ImageGenerationRequest {
    ImageGenerationRequest {
        prompt: "generate".to_owned(),
        n: Some(n),
        size: None,
        response_format: Some(format.to_owned()),
        reference_images: None,
        model: None,
    }
}

fn service(dir: &TempDir) -> DefaultImageGenService<LocalMediaStore, NoopUpload> {
    DefaultImageGenService::with_downloader(
        Arc::new(PayloadAdapter),
        LocalMediaStore::new(dir.path()),
        Arc::new(NoopUpload),
        "model",
        Arc::new(FixedDownloader),
    )
}

#[tokio::test]
async fn n_limits_results_and_url_and_b64_share_validated_bytes() {
    let url_dir = TempDir::new().unwrap();
    let url_service = service(&url_dir);
    let url_response = url_service.generate(request(1, "url")).await.unwrap();
    assert_eq!(url_response.data.len(), 1);
    let id = url_response.data[0]
        .url
        .as_deref()
        .unwrap()
        .trim_start_matches("/v1/images/");
    assert_eq!(url_service.get_image(id).await.unwrap().0.as_ref(), PNG);

    let b64_dir = TempDir::new().unwrap();
    let b64_response = service(&b64_dir)
        .generate(request(1, "b64_json"))
        .await
        .unwrap();
    let decoded = B64
        .decode(b64_response.data[0].b64_json.as_ref().unwrap())
        .unwrap();
    assert_eq!(decoded, PNG);
    assert!(url_response.data[0].b64_json.is_none());
    assert!(b64_response.data[0].url.is_none());
}

#[tokio::test]
async fn n_zero_is_rejected_cleanly() {
    let dir = TempDir::new().unwrap();
    assert!(matches!(
        service(&dir).generate(request(0, "url")).await,
        Err(ImageGenError::Validation(_))
    ));
}
