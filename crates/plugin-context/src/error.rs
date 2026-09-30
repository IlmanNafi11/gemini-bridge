use thiserror::Error;

/// Errors produced by the plugin system.
#[derive(Debug, Error)]
pub enum PluginError {
    #[error("Missing required service dependency: {0}")]
    MissingDependency(&'static str),
    #[error("Plugin setup failed: {0}")]
    SetupFailed(String),
    #[error("Event handler error: {0}")]
    HandlerError(String),
}
