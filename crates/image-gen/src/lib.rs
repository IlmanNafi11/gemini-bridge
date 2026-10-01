//! Image generation pipeline for gemini-bridge.
//!
//! Implements `POST /v1/images/generations` and `GET /v1/images/{id}` by:
//! 1. Resolving reference images through `upload`.
//! 2. Dispatching a generation request to the LLM adapter as a text prompt.
//! 3. Extracting `googleusercontent.com` image URLs from the response via regex.
//! 4. Downloading and caching image bytes in `media-store`.
//! 5. Returning bridge-proxied stable URLs or base64-encoded bytes.

pub mod extractor;

use std::sync::Arc;

use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;
use url::Url;

use gemini_bridge_llm_service::{
    ContentPart, LlmAdapter, LlmRequest, Message, ModelSelector, Role,
};
use gemini_bridge_media_store::{MediaMetadata, MediaStore};
use gemini_bridge_upload::UploadService;

use crate::extractor::ImageExtractor;

// ── Public types ─────────────────────────────────────────────────────────────

/// OpenAI-compatible image generation request body.
#[derive(Debug, Clone, Deserialize)]
pub struct ImageGenerationRequest {
    /// User prompt describing the image to generate.
    pub prompt: String,
    /// Number of images to generate. Default 1. Gemini Web may return only 1.
    pub n: Option<u32>,
    /// Requested dimensions. Passed through for spec compliance; Gemini ignores it.
    pub size: Option<String>,
    /// Output format: `"url"` (default) | `"b64_json"`.
    pub response_format: Option<String>,
    /// Optional reference image inputs.
    pub reference_images: Option<Vec<ImageInput>>,
    /// Model alias to forward to the adapter (e.g. `"gemini-web-flash"`).
    pub model: Option<String>,
}

/// A single reference image in one of the accepted forms.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ImageInput {
    /// HTTP(S) URL to a reference image.
    Url(String),
    /// Inline base64-encoded image with optional MIME-type hint.
    B64Json {
        b64_json: String,
        mime_type: Option<String>,
    },
    /// ID of a file previously uploaded via `POST /v1/files`.
    FileId(String),
}

/// OpenAI-compatible image generation response.
#[derive(Debug, Clone, Serialize)]
pub struct ImageGenerationResponse {
    /// Unix timestamp of image creation.
    pub created: i64,
    /// One or more generated image results.
    pub data: Vec<ImageResult>,
}

/// A single generated image result.
#[derive(Debug, Clone, Serialize)]
pub struct ImageResult {
    /// Bridge-proxied URL (`/v1/images/{id}`). Present when `response_format = "url"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Standard base64-encoded image bytes. Present when `response_format = "b64_json"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub b64_json: Option<String>,
}

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum ImageGenError {
    #[error("Reference upload failed: {0}")]
    UploadFailed(String),
    #[error("No generated image found in upstream response")]
    NoImageExtracted,
    #[error("Upstream image download failed: {0}")]
    ImageDownloadFailed(String),
    #[error("Media store error: {0}")]
    StoreError(String),
    #[error("Request validation failed: {0}")]
    Validation(String),
    #[error("Upstream adapter error: {0}")]
    AdapterError(String),
}

// ── Service trait ─────────────────────────────────────────────────────────────

#[async_trait]
pub trait ImageGenService: Send + Sync {
    /// Resolve references, generate image(s), cache results, and return a
    /// stable response.  Never returns raw `googleusercontent.com` URLs.
    async fn generate(
        &self,
        req: ImageGenerationRequest,
    ) -> Result<ImageGenerationResponse, ImageGenError>;

    /// Retrieve a previously generated and cached image by its local ID.
    /// Returns `(content_bytes, mime_type)`.
    async fn get_image(&self, id: &str) -> Result<(Bytes, String), ImageGenError>;
}

// ── Service implementation ────────────────────────────────────────────────────

/// Default implementation of [`ImageGenService`].
pub struct DefaultImageGenService<S, U> {
    adapter: Arc<dyn LlmAdapter>,
    store: S,
    upload: Arc<U>,
    /// Default model alias forwarded to the adapter.
    default_model: String,
}

impl<S, U> DefaultImageGenService<S, U>
where
    S: MediaStore + Clone + 'static,
    U: UploadService + 'static,
{
    pub fn new(
        adapter: Arc<dyn LlmAdapter>,
        store: S,
        upload: Arc<U>,
        default_model: impl Into<String>,
    ) -> Self {
        Self {
            adapter,
            store,
            upload,
            default_model: default_model.into(),
        }
    }

    /// Resolve a single [`ImageInput`] to a Gemini `fileRef` string.
    async fn resolve_reference(&self, input: &ImageInput) -> Result<String, ImageGenError> {
        match input {
            ImageInput::Url(raw) => {
                let url = Url::parse(raw)
                    .map_err(|e| ImageGenError::UploadFailed(format!("invalid URL: {e}")))?;
                let file = self
                    .upload
                    .upload_from_url(url)
                    .await
                    .map_err(|e| ImageGenError::UploadFailed(e.to_string()))?;
                Ok(file.file_ref)
            }
            ImageInput::B64Json {
                b64_json,
                mime_type: _,
            } => {
                let raw_bytes = B64
                    .decode(b64_json)
                    .map_err(|e| ImageGenError::UploadFailed(format!("invalid base64: {e}")))?;
                let file = self
                    .upload
                    .upload_bytes(Bytes::from(raw_bytes), None)
                    .await
                    .map_err(|e| ImageGenError::UploadFailed(e.to_string()))?;
                Ok(file.file_ref)
            }
            ImageInput::FileId(id) => {
                // Try by local ID first; the UploadedFile carries the fileRef.
                if let Some(file) = self.upload.get_by_id(id).await
                    && !file.file_ref.is_empty()
                {
                    return Ok(file.file_ref);
                }
                // Fall back to the store: the fileRef lives in media metadata.
                match self.store.get(id).await {
                    Ok((_, meta)) if !meta.file_ref.is_empty() => Ok(meta.file_ref),
                    Ok(_) => Err(ImageGenError::UploadFailed(format!(
                        "file {id} has no upstream fileRef"
                    ))),
                    Err(e) => Err(ImageGenError::UploadFailed(format!(
                        "file {id} not found: {e}"
                    ))),
                }
            }
        }
    }

    fn build_parts(prompt: String, file_refs: Vec<String>) -> Arc<[ContentPart]> {
        let mut parts = Vec::with_capacity(1 + file_refs.len());
        parts.push(ContentPart::Text(prompt));
        parts.extend(file_refs.into_iter().map(ContentPart::ImageRef));
        Arc::from(parts)
    }

    /// Download image bytes from a URL returned by the extractor.
    async fn download_image(url: &str) -> Result<Bytes, ImageGenError> {
        // We're fetching from Google's own CDN — no SSRF validation required.
        let client = reqwest::Client::builder()
            .build()
            .map_err(|e| ImageGenError::ImageDownloadFailed(e.to_string()))?;
        let response = client
            .get(url)
            .send()
            .await
            .map_err(|e| ImageGenError::ImageDownloadFailed(e.to_string()))?;
        if !response.status().is_success() {
            return Err(ImageGenError::ImageDownloadFailed(format!(
                "HTTP {}",
                response.status()
            )));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|e| ImageGenError::ImageDownloadFailed(e.to_string()))?;
        Ok(bytes)
    }

    fn unix_now() -> i64 {
        gemini_bridge_media_store::cleanup::now_unix()
    }
}

#[async_trait]
impl<S, U> ImageGenService for DefaultImageGenService<S, U>
where
    S: MediaStore + Clone + 'static,
    U: UploadService + 'static,
{
    async fn generate(
        &self,
        req: ImageGenerationRequest,
    ) -> Result<ImageGenerationResponse, ImageGenError> {
        // Validate prompt.
        if req.prompt.trim().is_empty() {
            return Err(ImageGenError::Validation("prompt must not be empty".into()));
        }

        // 1. Resolve reference images → fileRefs.
        let mut file_refs: Vec<String> = Vec::new();
        if let Some(refs) = &req.reference_images {
            for input in refs {
                let file_ref = self.resolve_reference(input).await?;
                file_refs.push(file_ref);
            }
        }

        // 2. Build a provider-neutral LLM request.
        let prompt_text = req.prompt.clone();
        let model_name = req
            .model
            .as_deref()
            .unwrap_or(&self.default_model)
            .to_owned();
        let llm_request = Arc::new(LlmRequest {
            model: ModelSelector {
                provider: "gemini-web".to_owned(),
                model: model_name.clone(),
                thinking_level: None,
            },
            messages: Arc::from(vec![Message {
                role: Role::User,
                parts: Self::build_parts(prompt_text, file_refs),
            }]),
            temperature: None,
            max_output_tokens: None,
            tools: Arc::from(vec![]),
            metadata: Default::default(),
        });

        // 3. Dispatch to adapter while preserving the provider response tree.
        let payload = self
            .adapter
            .complete_raw(llm_request)
            .await
            .map_err(|e| ImageGenError::AdapterError(e.to_string()))?;

        // 4. Extract image URLs from structured payload, with raw-text fallback.
        let mut image_urls = ImageExtractor::extract_image_urls(&payload);
        if image_urls.is_empty() {
            let raw = serde_json::to_string(&payload)
                .map_err(|e| ImageGenError::AdapterError(e.to_string()))?;
            image_urls = ImageExtractor::regex_fallback(&raw);
        }
        if image_urls.is_empty() {
            return Err(ImageGenError::NoImageExtracted);
        }

        let use_b64 = req.response_format.as_deref().unwrap_or("url") == "b64_json";

        let created = Self::unix_now();
        let mut results = Vec::with_capacity(image_urls.len());

        for url in &image_urls {
            // 5. Download image bytes.
            let image_bytes = Self::download_image(url).await?;

            // Detect MIME type from magic bytes; fall back to a safe default.
            let mime_type = gemini_bridge_upload::detect_mime(&image_bytes)
                .unwrap_or("image/png")
                .to_owned();

            // 6. Cache in media-store with generation metadata.
            let metadata = MediaMetadata {
                id: String::new(),
                sha256: String::new(),
                mime_type: mime_type.clone(),
                size_bytes: image_bytes.len() as u64,
                created_at: created,
                expires_at: None,
                prompt: Some(req.prompt.clone()),
                file_ref: String::new(),
                model: Some(model_name.clone()),
            };
            let id = self
                .store
                .put(image_bytes.clone(), metadata)
                .await
                .map_err(|e| ImageGenError::StoreError(e.to_string()))?;

            // 7. Build result — never return the raw googleusercontent.com URL.
            if use_b64 {
                results.push(ImageResult {
                    url: None,
                    b64_json: Some(B64.encode(&image_bytes)),
                });
            } else {
                results.push(ImageResult {
                    url: Some(format!("/v1/images/{id}")),
                    b64_json: None,
                });
            }
        }

        Ok(ImageGenerationResponse {
            created,
            data: results,
        })
    }

    async fn get_image(&self, id: &str) -> Result<(Bytes, String), ImageGenError> {
        let (bytes, metadata) = self
            .store
            .get(id)
            .await
            .map_err(|e| ImageGenError::StoreError(e.to_string()))?;
        // Log a soft warning for metadata anomalies but never expose store internals.
        if metadata.mime_type.is_empty() {
            warn!(image_id = %id, "media-store record has empty mime_type");
        }
        Ok((bytes, metadata.mime_type))
    }
}
