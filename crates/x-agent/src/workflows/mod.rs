//! Saved desktop workflows: typed, validated, permission-aware lists of desktop steps that
//! run by name or trigger phrase. See [`model`] for the format, [`engine`] for execution,
//! [`store`] for the files on disk.

pub mod engine;
pub mod model;
pub mod paths;
mod steps;
pub mod store;
pub mod system;

pub use engine::{Config, Host, Outcome, Report, RunState, StepStatus, run, undo};
pub use model::{Problem, Step, StepAction, Workflow};
pub use steps::preflight;
pub(crate) use steps::status_word;
pub use store::{Match, Store};

#[cfg(test)]
pub(crate) mod tests;
