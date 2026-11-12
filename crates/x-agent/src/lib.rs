//! X: the agent layer of the Hex fork.
//!
//! * [`tools`]: the typed, allowlisted Actions registry and rule planner. Nothing here
//!   spawns a shell; model output is untrusted data validated against these types.
//! * [`opencode`]: a supervised `opencode run` with a real event stream and a real Stop.
//! * [`desktop`] and [`computer`]: reading an app's accessibility tree and the bounded
//!   observe, act, verify loop that operates it.
//! * [`hexcli`]: Hex's own speech-to-text over its control CLI (never pastes).
//! * [`service`]: the one-task-at-a-time state machine the UI drives.

pub mod clipboard;
pub mod computer;
pub mod context;
pub mod desktop;
pub mod edit;
pub mod hexcli;
pub mod opencode;
pub mod phase;
pub mod planner;
pub mod quick;
pub mod service;
pub mod tools;
pub mod wm;
pub mod workflows;

/// Retry a spawn that fails with ETXTBSY: another thread forked while the executable was
/// still open for writing (only happens right after an executable is created).
pub(crate) fn retry_busy<T>(mut f: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut tries = 0;
    loop {
        match f() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < 20 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            other => return other,
        }
    }
}
