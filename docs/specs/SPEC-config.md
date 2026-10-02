# Module Specification: `config`

**Module ID:** `config`  
**Crate:** `gemini-bridge-config` (`crates/config`)  
**Phase:** Fase 0 (Foundation)  
**Parent Spec:** `SPEC.md` §2.1  
**Status:** Approved Draft  

---

## 1. Objective & Responsibility

The `config` module loads, validates, and merges configuration from `bridge.toml`, layered profiles (`dev`, `prod`, `lowmem`), and environment variable overrides (`BRIDGE_*`). It provides a strongly typed `BridgeConfig` structure accessible throughout the workspace.

---

## 2. Public API & Interfaces

```rust
use serde::{Deserialize, Serialize};
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
    #[error("Invalid configuration composition: {0}")]
    CompositionError(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerConfig {
    #[serde(default = "default_bind_addr")]
    pub bind_addr: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub api_key: Option<String>,
    #[serde(default)]
    pub cors_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageConfig {
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default = "default_media_ttl_days")]
    pub media_ttl_days: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransportConfig {
    #[serde(default = "default_tls_profile")]
    pub tls_profile: String, // "chrome", "firefox", "safari"
    pub proxy_url: Option<String>,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeConfig {
    pub server: ServerConfig,
    pub storage: StorageConfig,
    pub transport: TransportConfig,
}

impl BridgeConfig {
    pub fn load_from_file(path: &std::path::Path) -> Result<Self, ConfigError>;
    pub fn load_with_overrides(
        path: Option<&std::path::Path>,
        profile: Option<&str>,
    ) -> Result<Self, ConfigError>;
    pub fn load_composed(
        path: Option<&std::path::Path>,
        composition: &Composition,
    ) -> Result<Self, ConfigError>;
}

#[derive(Debug, Clone, Default)]
pub struct Composition {
    // Selected names are applied in insertion order.
}

impl Composition {
    pub fn new() -> Self;
    pub fn with_bundle(self, name: impl Into<String>) -> Self;
    pub fn with_profile(self, name: impl Into<String>) -> Self;
    pub fn with_patch(self, name: impl Into<String>) -> Self;
}
```

Composition TOML uses `[profiles.<name>]`, `[patches.<name>]`, and
`[bundles.<name>]` tables. Each bundle may declare ordered `profiles = [...]`
and `patches = [...]` lists. The legacy `[profile.<name>]` table remains
accepted when no plural `[profiles]` table is present.

Precedence, lowest to highest: defaults, base TOML, bundle-selected profiles
in bundle/list order, explicitly selected profiles in selection order,
bundle-selected patches in bundle/list order, explicitly selected patches in
selection order, and `BRIDGE_*` environment overrides.

---

## 3. Behavior & Invariants

1. **Composition Precedence:** Defaults < base TOML < bundle profiles < explicit profiles < bundle patches < explicit patches < `BRIDGE_*` environment overrides. Selected names apply in insertion order.
2. **Invalid Compositions:** Reject unknown selected names, missing bundle references, duplicate profile/patch selections, and simultaneous `dev`/`prod` selection. An unknown field in a selected overlay is rejected with the overlay name and field path.
3. **Sensible Defaults:**
   - Bind address: `127.0.0.1` (never `0.0.0.0` by default).
   - Port: `8090`.
   - Data directory: `~/.local/share/gemini-bridge/` (XDG compliant).
   - TLS Profile: `chrome`.
4. **Security Invariant:** API keys or passwords in the config struct must have `Debug` output masked.

---

## 4. Testing Strategy

- **Unit Tests:**
  - Parsing default TOML without optional fields.
  - Ordered profile, bundle, and patch precedence including bundle-member references.
  - Environment override precedence over the full composition (`BRIDGE_SERVER_PORT=9000`).
  - Clear rejection of unknown names/references, duplicate selections, conflicting `dev`/`prod` profiles, unknown overlay fields, and invalid values (e.g. port `0`).
- **Quality Gate:** 100% test pass on clippy and unit suite.

---

## 5. Boundaries

- **Always:** Enforce `127.0.0.1` as default bind host; validate directory paths before use.
- **Ask First:** Adding new top-level configuration sections or renaming environment prefix.
- **Never:** Log plain API keys or unencrypted passwords from configuration structs.
