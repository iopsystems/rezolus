// Stability analysis module
// Contains Allan variance, Hadamard variance, and Modified Allan variance analyses
// for characterizing noise and stability in time series data

mod allan;
mod common;
mod hadamard;
mod modified_allan;

// Re-export public types
pub use allan::AllanAnalysis;
pub use common::{CycleMinima, NoiseType};
pub use hadamard::HadamardAnalysis;
pub use modified_allan::ModifiedAllanAnalysis;

// Re-export analysis functions for use by parent module
pub(super) use allan::perform_allan_analysis;
pub(super) use hadamard::perform_hadamard_analysis;
pub(super) use modified_allan::perform_modified_allan_analysis;
