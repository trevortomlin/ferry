// A string that tries not to linger in memory or leak into logs.
//
// Best effort, and worth being honest about the limits: `String` may have
// reallocated while it was being typed, leaving copies of earlier prefixes on
// the heap that we cannot reach. What this does guarantee is that the final
// buffer is overwritten on drop, and that a `Secret` cannot be printed by
// accident — which is where credentials actually escape in practice.

use std::sync::atomic::{Ordering, compiler_fence};

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Deliberately verbose at the call site: every use should be easy to find.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // SAFETY: zero is valid UTF-8, and the string is dropped immediately
        // after, so no one observes the intermediate value.
        let bytes = unsafe { self.0.as_bytes_mut() };
        for byte in bytes {
            // Volatile so the optimiser cannot decide this write is dead.
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        compiler_fence(Ordering::SeqCst);
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.is_empty() {
            f.write_str("Secret(empty)")
        } else {
            write!(f, "Secret({} chars, redacted)", self.0.len())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_reveals_the_value() {
        let secret = Secret::new("hunter2");
        let rendered = format!("{secret:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(rendered.contains("redacted"));
    }

    #[test]
    fn debug_distinguishes_empty_from_set() {
        assert_eq!(format!("{:?}", Secret::default()), "Secret(empty)");
        assert!(Secret::default().is_empty());
        assert!(!Secret::new("x").is_empty());
    }

    #[test]
    fn expose_returns_the_value() {
        assert_eq!(Secret::new("hunter2").expose(), "hunter2");
    }
}
