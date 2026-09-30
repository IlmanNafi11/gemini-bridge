use crate::error::PluginError;
use crate::plugin::Disposer;
use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

/// Dependency-injection container, disposer registry, and event-bus holder
/// for the plugin lifecycle.
pub struct PluginContext {
    services: HashMap<&'static str, Arc<dyn Any + Send + Sync>>,
    disposers: Vec<Disposer>,
}

impl PluginContext {
    /// Create an empty context.
    pub fn new() -> Self {
        Self {
            services: HashMap::new(),
            disposers: Vec::new(),
        }
    }

    /// Register (or overwrite) a service under `key`.
    pub fn provide<T: Send + Sync + 'static>(&mut self, key: &'static str, service: Arc<T>) {
        self.services
            .insert(key, service as Arc<dyn Any + Send + Sync>);
    }

    /// Retrieve a service by key, downcasting to `Arc<T>`.
    ///
    /// Returns [`PluginError::MissingDependency`] when the key is absent or
    /// the stored value cannot be downcast to `T`.
    pub fn inject<T: Send + Sync + 'static>(
        &self,
        key: &'static str,
    ) -> Result<Arc<T>, PluginError> {
        self.services
            .get(key)
            .and_then(|arc| arc.clone().downcast::<T>().ok())
            .ok_or(PluginError::MissingDependency(key))
    }

    /// Push a disposer onto the LIFO stack.
    pub fn register_disposer(&mut self, disposer: Disposer) {
        self.disposers.push(disposer);
    }

    /// Run all disposers in LIFO order.  All disposers are called even when
    /// earlier ones fail; the first error encountered is returned.
    pub async fn teardown(&mut self) -> Result<(), PluginError> {
        let mut first_error: Option<PluginError> = None;
        while let Some(disposer) = self.disposers.pop() {
            if let Err(e) = disposer()
                && first_error.is_none()
            {
                first_error = Some(e);
            }
        }
        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl Default for PluginContext {
    fn default() -> Self {
        Self::new()
    }
}
