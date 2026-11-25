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

    /// Pin a standalone native window operation before it is shown for review.
    /// Legacy hosts preserve their close hook and cannot invent other identities.
    fn prepare_window(
        &self,
        request: &crate::wm::Request,
    ) -> Result<Option<crate::wm::WindowTarget>, String> {
        if request.op == crate::wm::Op::Close {
            self.prepare_close(&request.target)
        } else {
            Ok(None)
        }
    }

    fn prepare_window_choice(
        &self,
        _reference: &crate::context::WindowRef,
    ) -> Result<crate::wm::WindowTarget, String> {
        Err("This desktop cannot revalidate displayed window choices".into())
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

    fn prepare_window(
        &self,
        request: &crate::wm::Request,
    ) -> Result<Option<crate::wm::WindowTarget>, String> {
        if matches!(
            request.op,
            crate::wm::Op::List
                | crate::wm::Op::Keys
                | crate::wm::Op::Workspace
                | crate::wm::Op::Undo
        ) {
            return Ok(None);
        }
        crate::wm::prepare_window(request)
            .map(Some)
            .map_err(|error| error.to_string())
    }

    fn prepare_window_choice(
        &self,
        reference: &crate::context::WindowRef,
    ) -> Result<crate::wm::WindowTarget, String> {
        crate::wm::prepare_window_choice(reference).map_err(|error| error.to_string())
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
