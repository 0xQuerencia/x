//! Preparation binds a native operation to the identities actually reviewed.
//! Resolving a displayed ordinal reads fresh state and never executes anything.

use super::*;

pub(super) struct OperationScope {
    stop: crate::cancellation::Cancellation,
    expires: Option<Instant>,
}

fn stable(window: &WinInfo) -> Result<(), WmError> {
    if window.pid == 0 || window.id.is_empty() || window.class.is_empty() {
        return Err(WmError::Unsupported(
            "The desktop did not provide a stable window identity".into(),
        ));
    }
    Ok(())
}

impl Controller {
    pub(crate) fn run_exact_scoped(
        &mut self,
        op: Op,
        window: &WinInfo,
        workspace: Option<i64>,
        stop: &crate::cancellation::Cancellation,
    ) -> Result<(), WmError> {
        stop.check().map_err(WmError::Unverified)?;
        let ws = match (op.clone(), workspace) {
            (Op::Focus | Op::Close, None) => None,
            (Op::Move, Some(workspace)) if WORKSPACES.contains(&workspace) => {
                Some(Ws::N(workspace as u8))
            }
            _ => {
                return Err(WmError::Unsupported(
                    "Unsupported exact window operation".into(),
                ));
            }
        };
        let target = self.prepare_window_choice(&window.into())?;
        stop.check().map_err(WmError::Unverified)?;
        let outcome = self.run_with_target_scoped(
            &Request {
                op,
                target: Target {
                    app: window.class.clone(),
                    pick: None,
                },
                ws,
            },
            Some(&target),
            stop,
        )?;
        if outcome.effect == Effect::Confirmed {
            Ok(())
        } else {
            Err(WmError::Unverified(outcome.message))
        }
    }

    pub(crate) fn run_with_target_scoped(
        &mut self,
        request: &Request,
        target: Option<&WindowTarget>,
        stop: &crate::cancellation::Cancellation,
    ) -> Result<Outcome, WmError> {
        let expires = match target {
            Some(WindowTarget::Exact(target)) => Some(target.expires),
            _ => None,
        };
        self.operation_scope = Some(OperationScope {
            stop: stop.clone(),
            expires,
        });
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.run_with_target(request, target)
        }));
        self.operation_scope = None;
        match result {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    pub(super) fn check_operation_scope(&self) -> Result<(), WmError> {
        if let Some(scope) = &self.operation_scope {
            scope.stop.check().map_err(WmError::Unverified)?;
            if scope
                .expires
                .is_some_and(|expires| Instant::now() >= expires)
            {
                return Err(WmError::NotFound(
                    "The reviewed window reference expired".into(),
                ));
            }
        }
        Ok(())
    }

    pub(super) fn prepared_window(
        &self,
        target: &WindowTarget,
        all: &[WinInfo],
        focused: Option<&WinInfo>,
        strict_focus: bool,
    ) -> Result<WinInfo, WmError> {
        match target {
            WindowTarget::Exact(prepared) => {
                if Instant::now() >= prepared.expires {
                    return Err(WmError::NotFound(
                        "The reviewed window reference expired".into(),
                    ));
                }
                let found = if strict_focus {
                    focused.filter(|win| win.same_identity(&prepared.window))
                } else {
                    all.iter().find(|win| win.same_identity(&prepared.window))
                };
                found
                    .filter(|_| prepared.backend == self.backend.name())
                    .cloned()
                    .ok_or_else(|| WmError::NotFound("The approved window changed".into()))
            }
            WindowTarget::Choose(choices) => Err(WmError::Ambiguous(choices.clone())),
        }
    }

    pub(crate) fn prepare_window(&mut self, request: &Request) -> Result<WindowTarget, WmError> {
        let cap = match request.op {
            Op::Focus | Op::Back => Cap::Focus,
            Op::Move => Cap::MoveToWorkspace,
            Op::Maximize => Cap::Maximize,
            Op::Minimize => Cap::Minimize,
            Op::Close => Cap::Close,
            _ => {
                return Err(WmError::Unsupported(
                    "This operation has no window referent".into(),
                ));
            }
        };
        if !self.backend.caps().contains(&cap) {
            return Err(WmError::Unsupported(format!(
                "{} cannot {cap:?} on this desktop",
                self.backend.name()
            )));
        }
        let all = self.backend.windows()?;
        let focused = self.focused_in(&all)?;
        self.note_focus(focused.as_ref());
        let result = if request.op == Op::Back {
            self.fresh(self.previous.clone(), &all, "No previous window")
        } else {
            self.resolve(
                &request.target,
                &all,
                focused.as_ref(),
                request.op == Op::Focus,
            )
        };
        match result {
            Ok(window) => {
                stable(&window)?;
                Ok(WindowTarget::Exact(TargetIdentity {
                    backend: self.backend.name(),
                    window,
                    expires: Instant::now() + CHOICE_TTL,
                }))
            }
            Err(WmError::Ambiguous(choices)) => Ok(WindowTarget::Choose(choices)),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn prepare_window_choice(
        &mut self,
        reference: &crate::context::WindowRef,
    ) -> Result<WindowTarget, WmError> {
        let window = self.exact(&reference.id, reference.pid, &reference.class)?;
        stable(&window)?;
        Ok(WindowTarget::Exact(TargetIdentity {
            backend: self.backend.name(),
            window,
            expires: Instant::now() + CHOICE_TTL,
        }))
    }
}

pub(crate) fn run_with_target_scoped(
    request: &Request,
    target: Option<&WindowTarget>,
    stop: &crate::cancellation::Cancellation,
) -> Result<Outcome, WmError> {
    with_controller(|controller| controller.run_with_target_scoped(request, target, stop))
}

pub(crate) fn run_exact_scoped(
    op: Op,
    window: &WinInfo,
    workspace: Option<i64>,
    stop: &crate::cancellation::Cancellation,
) -> Result<(), WmError> {
    with_controller(|controller| controller.run_exact_scoped(op, window, workspace, stop))
}

pub(crate) fn prepare_window(request: &Request) -> Result<WindowTarget, WmError> {
    with_controller(|controller| controller.prepare_window(request))
}

pub(crate) fn prepare_window_choice(
    reference: &crate::context::WindowRef,
) -> Result<WindowTarget, WmError> {
    with_controller(|controller| controller.prepare_window_choice(reference))
}
