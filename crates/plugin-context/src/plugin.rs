use crate::context::PluginContext;
use crate::error::PluginError;
use async_trait::async_trait;

/// A reversible side-effect callback executed during [`PluginContext::teardown`].
pub type Disposer = Box<dyn FnOnce() -> Result<(), PluginError> + Send + Sync>;

/// Core extension trait implemented by every plugin.
#[async_trait]
pub trait Plugin: Send + Sync {
    /// Stable identifier for this plugin.
    fn id(&self) -> &'static str;

    /// Keys of services this plugin requires to be present in the context
    /// before [`Plugin::setup`] is called.  Defaults to no requirements.
    fn requires(&self) -> Vec<&'static str> {
        Vec::new()
    }

    /// Initialise the plugin against `ctx` and return a [`Disposer`] that
    /// undoes the setup when the context is torn down.
    async fn setup(&self, ctx: &mut PluginContext) -> Result<Disposer, PluginError>;
}
