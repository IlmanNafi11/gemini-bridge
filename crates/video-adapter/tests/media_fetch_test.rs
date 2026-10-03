use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use gemini_bridge_adapter_video::{
    DefaultVideoService, VideoAdapter, VideoConfig, VideoError, VideoGenerationRequest,
    VideoService,
};
use gemini_bridge_media_store::LocalMediaStore;
use gemini_bridge_upload::{MediaDownloader, MediaKind, UploadError};
use tempfile::TempDir;
use url::Url;

const MP4: &[u8] = b"\x00\x00\x00\x18ftypisom\x00\x00\x00\x00isommp42";

struct FixedAdapter;

#[async_trait]
impl VideoAdapter for FixedAdapter {
    async fn generate_video_raw(
        &self,
        _request: &VideoGenerationRequest,
    ) -> Result<String, VideoError> {
        Ok("https://video.example/result.mp4".to_owned())
    }
}

struct FixedDownloader;

#[async_trait]
impl MediaDownloader for FixedDownloader {
    async fn download(&self, url: &Url, kind: MediaKind) -> Result<Bytes, UploadError> {
        assert_eq!(url.as_str(), "https://video.example/result.mp4");
        assert_eq!(kind, MediaKind::Video);
        Ok(Bytes::from_static(MP4))
    }
}

fn request(response_format: Option<&str>) -> VideoGenerationRequest {
    VideoGenerationRequest {
        prompt: "a secure video".to_owned(),
        duration_seconds: None,
        response_format: response_format.map(str::to_owned),
    }
}

#[tokio::test]
async fn url_and_b64_results_come_from_the_same_validated_bytes() {
    let first_dir = TempDir::new().unwrap();
    let first = DefaultVideoService::with_downloader(
        VideoConfig { enabled: true },
        Arc::new(FixedAdapter),
        LocalMediaStore::new(first_dir.path()),
        Arc::new(FixedDownloader),
    );
    let url_response = first.generate(request(Some("url"))).await.unwrap();
    let id = url_response.data[0]
        .url
        .as_deref()
        .unwrap()
        .trim_start_matches("/v1/videos/");
    assert_eq!(first.get_video(id).await.unwrap().0.as_ref(), MP4);

    let second_dir = TempDir::new().unwrap();
    let second = DefaultVideoService::with_downloader(
        VideoConfig { enabled: true },
        Arc::new(FixedAdapter),
        LocalMediaStore::new(second_dir.path()),
        Arc::new(FixedDownloader),
    );
    let b64_response = second.generate(request(Some("b64_json"))).await.unwrap();
    let decoded = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        b64_response.data[0].b64_json.as_deref().unwrap(),
    )
    .unwrap();
    assert_eq!(decoded, MP4);
    assert!(url_response.data[0].b64_json.is_none());
    assert!(b64_response.data[0].url.is_none());
}

struct InvalidDownloader;

#[async_trait]
impl MediaDownloader for InvalidDownloader {
    async fn download(&self, _url: &Url, _kind: MediaKind) -> Result<Bytes, UploadError> {
        Ok(Bytes::from_static(b"not video"))
    }
}

#[tokio::test]
async fn rejects_downloaders_that_return_unsupported_video_signatures() {
    let dir = TempDir::new().unwrap();
    let service = DefaultVideoService::with_downloader(
        VideoConfig { enabled: true },
        Arc::new(FixedAdapter),
        LocalMediaStore::new(dir.path()),
        Arc::new(InvalidDownloader),
    );
    assert!(matches!(
        service.generate(request(None)).await,
        Err(VideoError::MediaError(_))
    ));
}
