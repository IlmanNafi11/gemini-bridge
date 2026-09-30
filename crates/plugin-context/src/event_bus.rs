/// Dispatch strategy for events emitted on the [`EventBus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchMode {
    /// Observers receive the event concurrently; return values are ignored.
    Emit,
    /// Handlers execute sequentially, transforming the event payload in a
    /// pipeline.
    Waterfall,
    /// Handlers execute sequentially in registration order.
    Serial,
    /// Handlers execute concurrently with join.
    Parallel,
    /// Sequential execution stopping at the first handler returning an early
    /// result.
    Bail,
}

/// Typed event bus (placeholder — full implementation is out of scope for
/// Fase 0).
pub struct EventBus;

impl EventBus {
    pub fn new() -> Self {
        Self
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}
