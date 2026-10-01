# Module Specification: `plugin-context`

**Module ID:** `plugin-context`  
**Crate:** `gemini-bridge-plugin-context` (`crates/plugin-context`)  
**Phase:** Fase 0 (Foundation), Fase 3 (built-in service reload)
**Parent Spec:** `SPEC.md` §2.1
**Status:** Approved Draft — built-in instance replacement decision recorded in root SPEC.md §10

---

## 1. Objective & Responsibility

The `plugin-context` module provides the core dependency injection (DI) container, typed event bus, plugin lifecycle management, and reversible effect disposers for `gemini-bridge`. Following the "everything is a plugin" design philosophy, it allows services (transport, identity, store, adapters, middleware) to declare their dependencies and register lifecycle hooks deterministically without global mutable state or runtime panics.

---

## 2. Public API & Interfaces

```rust
use async_trait::async_trait;
use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("Missing required service dependency: {0}")]
    MissingDependency(&'static str),
    #[error("Plugin setup failed: {0}")]
    SetupFailed(String),
    #[error("Event handler error: {0}")]
    HandlerError(String),
}

/// Disposer callback executed when a plugin or effect is torn down.
pub type Disposer = Box<dyn FnOnce() -> Result<(), PluginError> + Send + Sync>;

/// Core Plugin trait implemented by modular extensions.
#[async_trait]
pub trait Plugin: Send + Sync {
    fn id(&self) -> &'static str;
    fn requires(&self) -> Vec<&'static str> { Vec::new() }
    async fn setup(&self, ctx: &mut PluginContext) -> Result<Disposer, PluginError>;
}

/// Typed Event Bus Dispatch Modes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchMode {
    /// Observers receive the event concurrently; return values are ignored.
    Emit,
    /// Handlers execute sequentially, transforming the event payload in a pipeline.
    Waterfall,
    /// Handlers execute sequentially in registration order.
    Serial,
    /// Handlers execute concurrently with join.
    Parallel,
    /// Sequential execution stopping at the first handler returning an early result.
    Bail,
}

pub struct PluginContext {
    services: HashMap<&'static str, Arc<dyn Any + Send + Sync>>,
    disposers: Vec<Disposer>,
    event_bus: EventBus,
}

impl PluginContext {
    pub fn new() -> Self;
    pub fn provide<T: Send + Sync + 'static>(&mut self, key: &'static str, service: Arc<T>);
    pub fn inject<T: Send + Sync + 'static>(&self, key: &'static str) -> Result<Arc<T>, PluginError>;
    pub fn register_disposer(&mut self, disposer: Disposer);
    pub async fn teardown(&mut self) -> Result<(), PluginError>;
}

/// Clonable slot for atomically replacing a built-in service generation.
pub struct ReloadableSlot<T: ?Sized>;
impl<T: ?Sized> ReloadableSlot<T> {
    pub fn new(initial: Arc<T>) -> Self;
    pub async fn get(&self) -> Arc<T>;
    pub async fn swap(&self, replacement: Arc<T>) -> Arc<T>;
}
```

---

## 3. Behavior & Invariants

1. **Service Registration:** Services are registered via static string keys (`&'static str`). Downcasting must be safe and return `PluginError::MissingDependency` if missing or type mismatched.
2. **Deterministic Teardown:** Disposers are executed in reverse order of registration (LIFO) during `teardown()`.
3. **Event Bus Immutability:** Events dispatched under `Waterfall` mode take an owned payload and return the modified payload. Events under `Emit`, `Parallel`, and `Serial` operate on immutable references.
4. **Thread Safety:** All container contents must satisfy `Send + Sync + 'static`.
5. **No Dynamic Loading:** The registry remains statically linked; Fase 3 reload replaces built-in instances only and uses safe `Arc` ownership to retain old generations while existing requests drain.

---

## 4. Testing Strategy

- **Unit Tests:**
  - `provide` and `inject` with multiple types, verifying correct downcasting and error on missing key.
  - Event bus dispatch modes (`Emit`, `Waterfall`, `Serial`, `Parallel`, `Bail`) with mock handler counters.
  - Teardown order verification (LIFO execution of disposers).
- **Quality Gate:** 100% test pass, zero warnings on clippy.
- Fase 3 reload publishes replacements synchronously only after asynchronous preparation completes; the short publish operation must not await or block an async runtime worker.

---

## 5. Boundaries

- **Always:** Redact sensitive context before logging; verify `Send + Sync` bounds on all registered services.
- **Ask First:** Changing public event bus dispatch modes or introducing runtime reflection crates.
- **Never:** Use `unsafe` blocks for downcasting or pointer manipulation; never panic in `inject()`.
