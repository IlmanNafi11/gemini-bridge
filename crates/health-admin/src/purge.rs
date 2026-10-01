//! Media purge service for administrative cleanup operations.

use async_trait::async_trait;
use gemini_bridge_media_store::MediaStore;
use serde::Serialize;
use std::sync::Arc;

use crate::readiness::HealthAdminError;

/// Response payload for media purge operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PurgeResult {
    /// Total number of expired metadata records and unreferenced content objects purged.
    pub purged: usize,
}

/// Administrative service for media purge operations.
#[async_trait]
pub trait MediaPurgeAdminService: Send + Sync {
    /// Purge expired media items from the backing store.
    async fn purge_expired(&self) -> Result<PurgeResult, HealthAdminError>;
}

/// Default implementation of [`MediaPurgeAdminService`] backed by [`MediaStore`].
pub struct DefaultMediaPurgeAdminService {
    store: Arc<dyn MediaStore>,
}

impl DefaultMediaPurgeAdminService {
    pub fn new(store: Arc<dyn MediaStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl MediaPurgeAdminService for DefaultMediaPurgeAdminService {
    async fn purge_expired(&self) -> Result<PurgeResult, HealthAdminError> {
        let count = self
            .store
            .purge_expired()
            .await
            .map_err(|e| HealthAdminError::PurgeFailed(format!("media purge failed: {e}")))?;
        Ok(PurgeResult { purged: count })
    }
}
