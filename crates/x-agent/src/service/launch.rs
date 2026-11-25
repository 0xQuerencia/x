//! Native launch and prepared-action boundary.

use super::*;

/// Launches validated actions (faked in tests).
pub trait Launcher: Send + Sync {
    fn launch(&self, action: &Action) -> Result<String, String>;

    fn open_project_folder(&self, _directory: &Path) -> Result<(), String> {
        Err("Opening project folders is unavailable".into())
    }

    fn prepare_close(
        &self,
        _target: &crate::wm::Target,
    ) -> Result<Option<crate::wm::CloseTarget>, String> {
        Ok(None)
    }

    fn launch_prepared(
        &self,
        action: &Action,
        _target: Option<&crate::wm::CloseTarget>,
        stop: &Cancellation,
    ) -> Result<String, String> {
        stop.check()?;
        self.launch(action)
    }

    /// A legacy launcher proves dispatch only, even if its message claims more.
    /// Native adapters override this to preserve independent readback evidence.
    fn execute_prepared(
        &self,
        action: &Action,
        target: Option<&crate::wm::CloseTarget>,
        stop: &Cancellation,
    ) -> Result<tools::ExecutionOutcome, String> {
        self.launch_prepared(action, target, stop)
            .map(tools::ExecutionOutcome::launched)
    }

    /// Waits for a window other than `before` to take focus, so typing lands in the app
    /// that was just launched. Errors when none appears.
    fn await_launched_focus(&self, stop: &Cancellation) -> Result<(), String> {
        stop.check()
    }
}

pub struct RealLauncher;

impl Launcher for RealLauncher {
    fn launch(&self, action: &Action) -> Result<String, String> {
        action.execute()
    }

    fn open_project_folder(&self, directory: &Path) -> Result<(), String> {
        crate::quick::files::launch_path(directory)
    }

    fn prepare_close(
        &self,
        target: &crate::wm::Target,
    ) -> Result<Option<crate::wm::CloseTarget>, String> {
        crate::wm::prepare_close(target)
            .map(Some)
            .map_err(|err| err.to_string())
    }

    fn launch_prepared(
        &self,
        action: &Action,
        target: Option<&crate::wm::CloseTarget>,
        stop: &Cancellation,
    ) -> Result<String, String> {
        action.execute_for_target(target, stop)
    }

    fn await_launched_focus(&self, stop: &Cancellation) -> Result<(), String> {
        tools::wait_for_launched_focus_scoped(stop)
    }

    fn execute_prepared(
        &self,
        action: &Action,
        target: Option<&crate::wm::CloseTarget>,
        stop: &Cancellation,
    ) -> Result<tools::ExecutionOutcome, String> {
        action.execute_outcome_for_target(target, stop)
    }
}
