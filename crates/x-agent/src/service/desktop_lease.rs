//! Serialize X-issued desktop effects across foreground commands and graph runs.
//! This does not lock out the person: native identities still require rechecking.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use uuid::Uuid;

use super::{Cancellation, Phase, Service, busy};

#[derive(Default)]
pub(super) struct Lock {
    owner: Mutex<Option<(Uuid, Option<u64>)>>,
}

pub(super) struct DesktopLease {
    lock: Arc<Lock>,
    claim: Uuid,
}

impl Drop for DesktopLease {
    fn drop(&mut self) {
        let mut owner = self.lock.owner.lock().unwrap_or_else(|p| p.into_inner());
        if owner
            .as_ref()
            .is_some_and(|(claim, _)| *claim == self.claim)
        {
            *owner = None;
        }
    }
}

impl Lock {
    fn try_acquire(self: &Arc<Self>, task_id: Option<u64>) -> Result<Option<DesktopLease>, String> {
        let mut owner = self
            .owner
            .lock()
            .map_err(|_| "Desktop coordinator unavailable")?;
        if owner.is_some() {
            return Ok(None);
        }
        let claim = Uuid::new_v4();
        *owner = Some((claim, task_id));
        Ok(Some(DesktopLease {
            lock: Arc::clone(self),
            claim,
        }))
    }
}

impl Service {
    /// Call before re-observing targets and keep the guard until the effect and
    /// its readback finish. Waiting is bounded and checks the original Stop.
    pub(super) fn acquire_desktop(
        &self,
        task_id: u64,
        stop: &Cancellation,
        timeout: Duration,
    ) -> Result<DesktopLease, String> {
        let started = Instant::now();
        loop {
            self.check_desktop_task(task_id, stop)?;
            if let Some(lease) = self.desktop_lock.try_acquire(Some(task_id))? {
                self.check_desktop_task(task_id, stop)?;
                return Ok(lease);
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err("Another X operation is using the desktop; no input was sent".into());
            }
            std::thread::sleep(remaining.min(Duration::from_millis(25)));
        }
    }

    fn check_desktop_task(&self, task_id: u64, stop: &Cancellation) -> Result<(), String> {
        stop.check()?;
        let state = self.lock();
        state.stop.check()?;
        if state.phase != Phase::Running
            || state.task.as_ref().is_none_or(|task| task.id != task_id)
        {
            return Err(
                "The desktop operation no longer belongs to the active approved task".into(),
            );
        }
        Ok(())
    }

    /// A direct native user control (Quick, an exact window choice, or Undo)
    /// uses the same exclusive slot without creating an automatic task grant.
    /// Refuse promptly if an owned task is active; UI handlers must not block.
    pub fn with_desktop_control<T>(
        &self,
        action: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        {
            let state = self.lock();
            if state.phase.is_busy() {
                return Err(busy(state.phase));
            }
        }
        let _lease = self.try_desktop_control()?;
        {
            let state = self.lock();
            if state.phase.is_busy() {
                return Err(busy(state.phase));
            }
        }
        action()
    }

    /// Internal instant operations have already bound their foreground task.
    /// This only acquires input exclusion; it cannot issue execution authority.
    pub(super) fn try_desktop_control(&self) -> Result<DesktopLease, String> {
        self.desktop_lock
            .try_acquire(None)?
            .ok_or_else(|| "Another X operation is using the desktop; no input was sent".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hexcli::{Stt, SttError};
    use crate::service::{Launcher, Mode};
    use crate::tools::Action;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct NoInput;
    impl Stt for NoInput {
        fn start(&self) -> Result<(), SttError> {
            unreachable!()
        }
        fn stop_print(&self) -> Result<String, SttError> {
            unreachable!()
        }
        fn cancel(&self) -> Result<(), SttError> {
            Ok(())
        }
    }
    impl Launcher for NoInput {
        fn launch(&self, _: &Action) -> Result<String, String> {
            unreachable!()
        }
    }
    fn service() -> Service {
        Service::new(Arc::new(NoInput), Arc::new(NoInput), None)
    }

    #[test]
    fn a_direct_control_never_runs_inside_another_desktop_effect() {
        let service = service();
        let mutations = AtomicUsize::new(0);
        service
            .with_desktop_control(|| {
                assert!(
                    service
                        .with_desktop_control(|| {
                            mutations.fetch_add(1, Ordering::SeqCst);
                            Ok(())
                        })
                        .is_err()
                );
                Ok(())
            })
            .unwrap();
        assert_eq!(mutations.load(Ordering::SeqCst), 0);
        service
            .with_desktop_control(|| {
                mutations.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .unwrap();
        assert_eq!(mutations.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn an_approved_desktop_wait_respects_stop_and_stale_task_identity() {
        let service = service();
        let stop = Cancellation::default();
        service
            .with_desktop_control(|| {
                // Keep the direct-control lease while the foreground's separately
                // approved task waits for input ownership.
                let task = service.plan("open firefox", Mode::Actions).unwrap();
                super::super::set_phase(&mut service.lock(), Phase::Running);
                assert!(
                    service
                        .acquire_desktop(task.id + 1, &stop, Duration::ZERO)
                        .is_err()
                );
                std::thread::scope(|scope| {
                    let waiting = scope.spawn(|| {
                        service
                            .acquire_desktop(task.id, &stop, Duration::from_secs(15))
                            .map(drop)
                    });
                    std::thread::sleep(Duration::from_millis(40));
                    let started = Instant::now();
                    stop.request();
                    assert_eq!(waiting.join().unwrap().unwrap_err(), "Stopped");
                    assert!(started.elapsed() < Duration::from_secs(1));
                });
                Ok(())
            })
            .unwrap();
        assert!(
            service
                .acquire_desktop(
                    service.lock().task.as_ref().unwrap().id,
                    &stop,
                    Duration::ZERO
                )
                .is_err()
        );
    }
}
