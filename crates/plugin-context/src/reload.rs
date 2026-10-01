//! Atomic built-in service instance replacement.
//!
//! Consumers clone a generation handle before beginning asynchronous work. A
//! reload publishes a new generation without invalidating handles held by
//! requests that are already in flight. A generation's disposer runs when its
//! final handle is dropped.

use std::ops::Deref;
use std::sync::{Arc, Mutex, RwLock};

use crate::Disposer;

struct Generation<T: ?Sized> {
    service: Arc<T>,
    disposer: Mutex<Option<Disposer>>,
}

impl<T: ?Sized> Drop for Generation<T> {
    fn drop(&mut self) {
        if let Ok(disposer) = self.disposer.get_mut()
            && let Some(disposer) = disposer.take()
            && let Err(error) = disposer()
        {
            tracing::error!(%error, "plugin generation disposer failed");
        }
    }
}

/// An owned reference to a plugin generation that remains valid across reloads.
pub struct PluginGeneration<T: ?Sized> {
    generation: Arc<Generation<T>>,
}

impl<T: ?Sized> Clone for PluginGeneration<T> {
    fn clone(&self) -> Self {
        Self {
            generation: Arc::clone(&self.generation),
        }
    }
}

impl<T: ?Sized> Deref for PluginGeneration<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.generation.service
    }
}

/// A clonable slot whose current built-in service instance can be replaced.
pub struct ReloadableSlot<T: ?Sized> {
    current: Arc<RwLock<Arc<Generation<T>>>>,
}

impl<T: ?Sized> Clone for ReloadableSlot<T> {
    fn clone(&self) -> Self {
        Self {
            current: Arc::clone(&self.current),
        }
    }
}

impl<T: ?Sized> ReloadableSlot<T> {
    /// Create a slot with its initial service generation and no disposer.
    pub fn new(initial: Arc<T>) -> Self {
        Self::new_with_disposer(initial, None)
    }

    /// Create a slot with its initial service generation and disposer.
    pub fn new_with_disposer(initial: Arc<T>, disposer: Option<Disposer>) -> Self {
        Self {
            current: Arc::new(RwLock::new(Arc::new(Generation {
                service: initial,
                disposer: Mutex::new(disposer),
            }))),
        }
    }

    /// Clone a handle to the currently published generation.
    pub async fn get(&self) -> PluginGeneration<T> {
        PluginGeneration {
            generation: Arc::clone(
                &self
                    .current
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            ),
        }
    }

    /// Synchronously and atomically publish a replacement generation.
    ///
    /// Intended for the commit phase of a two-phase reload workflow after all
    /// asynchronous preparation and initialization has completed.
    pub fn publish(&self, replacement: Arc<T>) -> PluginGeneration<T> {
        self.publish_with_disposer(replacement, None)
    }

    /// Synchronously publish a replacement with its lifecycle disposer.
    pub fn publish_with_disposer(
        &self,
        replacement: Arc<T>,
        disposer: Option<Disposer>,
    ) -> PluginGeneration<T> {
        let replacement = Arc::new(Generation {
            service: replacement,
            disposer: Mutex::new(disposer),
        });
        let old = {
            let mut current = self
                .current
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::replace(&mut *current, replacement)
        };
        PluginGeneration { generation: old }
    }
}
