use thiserror::Error;

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("No session credentials are configured")]
    MissingCredentials,
    #[error("Session is invalid or expired")]
    NeedsReauth,
    #[error("Gemini Web session was flagged by upstream")]
    IpFlagged,
    #[error("Bootstrap response is missing required field: {0}")]
    MissingBootstrapField(&'static str),
    #[error("Credential storage failed")]
    Storage,
    #[error("Transport request failed")]
    Transport,
}
