/// Ordered selection of named configuration bundles, profiles, and patches.
///
/// Resolution precedence is documented by [`BridgeConfig::load_composed`]: base
/// configuration is layered first, bundle/explicit profiles next, then patches,
/// and `BRIDGE_*` environment variables last.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Composition {
    pub(crate) bundles: Vec<String>,
    pub(crate) profiles: Vec<String>,
    pub(crate) patches: Vec<String>,
}

impl Composition {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_bundle(mut self, name: impl Into<String>) -> Self {
        self.bundles.push(name.into());
        self
    }

    pub fn with_profile(mut self, name: impl Into<String>) -> Self {
        self.profiles.push(name.into());
        self
    }

    pub fn with_patch(mut self, name: impl Into<String>) -> Self {
        self.patches.push(name.into());
        self
    }
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct BundleDefinition {
    pub profiles: Vec<String>,
    pub patches: Vec<String>,
}
