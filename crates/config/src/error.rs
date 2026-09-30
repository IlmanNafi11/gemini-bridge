use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Configuration file not found: {0}")]
    FileNotFound(PathBuf),
    #[error("Parse error: {0}")]
    ParseError(Box<figment::Error>),
    #[error("Validation failed: {0}")]
    ValidationError(String),
}
impl From<figment::Error> for ConfigError {
    fn from(error: figment::Error) -> Self {
        Self::ParseError(Box::new(error))
    }
}
