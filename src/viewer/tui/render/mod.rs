//! ratatui rendering. Each screen is a free function taking a `Frame`
//! and the `App`. Kept separate from state so widgets stay dumb.

pub mod browser;
pub mod chart;
