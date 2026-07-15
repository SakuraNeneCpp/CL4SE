//! OS-independent decision logic.
//!
//! The engine is intentionally only a placeholder in M0. Its behavior is added in M1.

pub mod tracker;

/// Placeholder passed across the platform trait boundary until M1.
#[derive(Debug, Default)]
pub struct Engine;
