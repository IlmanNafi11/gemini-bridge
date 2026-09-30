//! Prefix-diff engine for cumulative streaming text.
//!
//! Gemini Web's `StreamGenerate` endpoint emits cumulative snapshots of the
//! response text — each frame contains the *full* text accumulated so far, not
//! just the new part. This module converts those snapshots into incremental
//! deltas suitable for SSE emission.
//!
//! ## Behaviour
//!
//! * **Monotonic extension:** when the new snapshot extends the current
//!   buffer, emit only the new suffix.
//! * **Safe-reset on regression:** when a new snapshot is *shorter than* or
//!   does *not share a prefix with* the current buffer (upstream rewrote earlier
//!   text, e.g. thinking-model branch revision), the engine performs a safe
//!   reset: it emits the entire new snapshot as a replacement delta and resets
//!   its internal buffer to that snapshot.

#[derive(Debug, Default)]
pub struct PrefixDiff {
    /// The full cumulative text seen so far.
    buffer: String,
}

/// The kind of delta produced by one [`PrefixDiff::update`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delta {
    /// The new text appended to the buffer — the common streaming case.
    Suffix(String),
    /// The upstream rewrote the text; the entire new snapshot is a replacement.
    ///
    /// Consumers should discard anything they have emitted so far and treat
    /// this as the current full content.
    Reset(String),
}

impl PrefixDiff {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed the next cumulative snapshot; returns the delta to emit.
    ///
    /// Returns `None` when the snapshot is identical to the current buffer
    /// (a no-op frame).
    pub fn update(&mut self, snapshot: &str) -> Option<Delta> {
        if snapshot == self.buffer {
            return None;
        }

        if snapshot.starts_with(self.buffer.as_str()) {
            // Monotonic extension — emit only the new suffix.
            let suffix = snapshot[self.buffer.len()..].to_owned();
            self.buffer = snapshot.to_owned();
            Some(Delta::Suffix(suffix))
        } else {
            // Upstream rewrote earlier text — safe reset.
            self.buffer = snapshot.to_owned();
            Some(Delta::Reset(snapshot.to_owned()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_snapshot_emits_full_suffix() {
        let mut diff = PrefixDiff::new();
        let delta = diff.update("Hello").unwrap();
        assert_eq!(delta, Delta::Suffix("Hello".to_owned()));
    }

    #[test]
    fn second_monotonic_snapshot_emits_only_new_text() {
        let mut diff = PrefixDiff::new();
        diff.update("Hello");
        let delta = diff.update("Hello world").unwrap();
        assert_eq!(delta, Delta::Suffix(" world".to_owned()));
    }

    #[test]
    fn identical_snapshot_returns_none() {
        let mut diff = PrefixDiff::new();
        diff.update("Hello");
        assert!(diff.update("Hello").is_none());
    }

    #[test]
    fn non_prefix_snapshot_triggers_safe_reset() {
        let mut diff = PrefixDiff::new();
        diff.update("Thinking...");
        let delta = diff.update("Final answer").unwrap();
        assert_eq!(delta, Delta::Reset("Final answer".to_owned()));
    }

    #[test]
    fn after_reset_subsequent_monotonic_extension_works() {
        let mut diff = PrefixDiff::new();
        diff.update("Old text");
        diff.update("New"); // reset
        let delta = diff.update("New text").unwrap();
        assert_eq!(delta, Delta::Suffix(" text".to_owned()));
    }

    #[test]
    fn empty_first_snapshot_emits_no_delta() {
        let mut diff = PrefixDiff::new();
        assert!(diff.update("").is_none());
    }

    #[test]
    fn update_from_empty_buffer_treats_as_suffix() {
        let mut diff = PrefixDiff::new();
        diff.update(""); // no-op
        let delta = diff.update("Hello").unwrap();
        assert_eq!(delta, Delta::Suffix("Hello".to_owned()));
    }
}
