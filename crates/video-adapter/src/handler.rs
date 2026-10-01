//! Default video generation service implementation.

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use bytes::Bytes;
use reqwest::Client;

use gemini_bridge_media_store::{MediaMetadata, MediaStore, cleanup};

use crate::{
    VideoAdapter, VideoConfig, VideoError, VideoGenerationRequest, VideoGenerationResponse,
    VideoResult, VideoService,
};

/// Default implementation of [`VideoService`].
pub struct DefaultVideoService<S, A> {
    config: VideoConfig,
    adapter: Arc<A>,
    store: S,
}

impl<S, A> DefaultVideoService<S, A>
where
    S: MediaStore + Clone + 'static,
    A: VideoAdapter + 'static,
{
    pub fn new(config: VideoConfig, adapter: Arc<A>, store: S) -> Self {
        Self {
            config,
            adapter,
            store,
        }
    }

    fn validate_request(req: &VideoGenerationRequest) -> Result<(), VideoError> {
        let prompt = req.prompt.trim();
        if prompt.is_empty() {
            return Err(VideoError::Validation(
                "prompt must not be empty".to_owned(),
            ));
        }

        if let Some(dur) = req.duration_seconds
            && !(1..=60).contains(&dur)
        {
            return Err(VideoError::Validation(
                "duration_seconds must be between 1 and 60".to_owned(),
            ));
        }

        if let Some(fmt) = &req.response_format
            && fmt != "url"
            && fmt != "b64_json"
        {
            return Err(VideoError::Validation(format!(
                "invalid response_format '{fmt}', must be 'url' or 'b64_json'"
            )));
        }

        Ok(())
    }

    async fn download_video_bytes(url: &str) -> Result<(Bytes, String), VideoError> {
        let client = Client::builder()
            .build()
            .map_err(|e| VideoError::MediaError(e.to_string()))?;
        let response = client
            .get(url)
            .send()
            .await
            .map_err(|e| VideoError::MediaError(e.to_string()))?;

        if !response.status().is_success() {
            return Err(VideoError::MediaError(format!(
                "HTTP {}",
                response.status()
            )));
        }

        let mime = response
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("video/mp4")
            .to_owned();

        let bytes = response
            .bytes()
            .await
            .map_err(|e| VideoError::MediaError(e.to_string()))?;

        Ok((bytes, mime))
    }
}

#[async_trait]
impl<S, A> VideoService for DefaultVideoService<S, A>
where
    S: MediaStore + Clone + 'static,
    A: VideoAdapter + 'static,
{
    async fn generate(
        &self,
        req: VideoGenerationRequest,
    ) -> Result<VideoGenerationResponse, VideoError> {
        if !self.config.enabled {
            return Err(VideoError::Disabled);
        }

        Self::validate_request(&req)?;

        let upstream_url = self.adapter.generate_video_raw(&req).await?;

        let (video_bytes, mime_type) = Self::download_video_bytes(&upstream_url).await?;

        let created = cleanup::now_unix();
        let metadata = MediaMetadata {
            id: String::new(),
            sha256: String::new(),
            mime_type,
            size_bytes: video_bytes.len() as u64,
            created_at: created,
            expires_at: None,
            prompt: Some(req.prompt.clone()),
            file_ref: String::new(),
            model: None,
        };

        let id = self
            .store
            .put(video_bytes.clone(), metadata)
            .await
            .map_err(|e| VideoError::MediaError(e.to_string()))?;

        let use_b64 = req.response_format.as_deref() == Some("b64_json");

        let result = if use_b64 {
            VideoResult {
                url: None,
                b64_json: Some(base64::engine::general_purpose::STANDARD.encode(&video_bytes)),
            }
        } else {
            VideoResult {
                url: Some(format!("/v1/videos/{id}")),
                b64_json: None,
            }
        };

        Ok(VideoGenerationResponse {
            created,
            data: vec![result],
        })
    }

    async fn get_video(&self, id: &str) -> Result<(Bytes, String), VideoError> {
        let (bytes, meta) = self
            .store
            .get(id)
            .await
            .map_err(|e| VideoError::MediaError(e.to_string()))?;

        let mime = if meta.mime_type.is_empty() {
            "video/mp4".to_owned()
        } else {
            meta.mime_type
        };

        Ok((bytes, mime))
    }
}
