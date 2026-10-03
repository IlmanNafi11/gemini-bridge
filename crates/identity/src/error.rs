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
    #[error(
        "Invalid BRIDGE_SECRET: configure at least 32 bytes with at least 8 distinct characters; generate one with `openssl rand -base64 32` (do not use an empty or placeholder value)"
    )]
    WeakBridgeSecret,
    #[error("Candidate credential validation is not supported by this identity provider")]
    CandidateValidationUnsupported,
    #[error("Credential storage failed")]
    Storage,
    #[error("Legacy credential rewrap failed; original ciphertext was preserved")]
    LegacyCredentialRewrap,
    #[error("Transport request failed")]
    Transport,
}
