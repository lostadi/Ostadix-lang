//! A cooperative cancellation token.
//!
//! Used by group `any`/`race` handling: once one member has produced the
//! selected result, the coordinator can signal siblings that their result is
//! no longer needed. Cancellation is cooperative — workers observe the flag at
//! safe points and stop early where practical; it never forcibly interrupts a
//! running computation.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A shareable, cheaply-clonable cancellation flag.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    flag: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Request cancellation. Idempotent.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_starts_uncancelled_and_latches() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());
        token.cancel();
        assert!(token.is_cancelled());
        let clone = token.clone();
        assert!(clone.is_cancelled());
    }
}
