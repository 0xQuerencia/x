//! Bounded local readiness for the next grounded decision. A changed redacted
//! observation is useful new context, never proof that an application or goal
//! has finished. No model request or input is issued while waiting.

use std::time::{Duration, Instant};

use super::{Host, rebind, wait_interruptible};
use crate::desktop::{Desktop, Observation, Win};

const POLL: Duration = Duration::from_millis(50);

pub(super) fn check_live(
    host: &dyn Host,
    started: Instant,
    deadline: Duration,
) -> Result<(), String> {
    if host.stopped() {
        return Err("Stopped".into());
    }
    if started.elapsed() >= deadline {
        return Err("Stopped: the time limit was reached".into());
    }
    Ok(())
}

pub(super) struct Observer<'a> {
    pub desktop: &'a dyn Desktop,
    pub app: &'a str,
    pub window: &'a Win,
    pub host: &'a dyn Host,
    pub started: Instant,
    pub deadline: Duration,
}

pub(super) struct Ready {
    pub window: Win,
    pub observation: Observation,
    pub timed_out: bool,
}

impl Observer<'_> {
    fn check_live(&self) -> Result<(), String> {
        check_live(self.host, self.started, self.deadline)
    }

    /// Return the first changed observation or the freshest one at the bounded
    /// wait limit. Snapshot-generation tokens alone do not count as change.
    /// Every poll rebinds the originally granted native window identity.
    pub fn after_change(&self, before: Option<&str>, max_wait: Duration) -> Result<Ready, String> {
        let waiting_since = Instant::now();
        loop {
            self.check_live()?;
            let window = rebind(self.desktop, self.app, self.window, self.host)?;
            self.check_live()?;
            let observation = self
                .desktop
                .observe(&window)
                .map_err(|why| format!("Could not read the window: {why}"))?;
            self.check_live()?;
            if before.is_none_or(|before| observation.render() != before) {
                return Ok(Ready {
                    window,
                    observation,
                    timed_out: false,
                });
            }
            let remaining = max_wait.saturating_sub(waiting_since.elapsed());
            if remaining.is_zero() {
                return Ok(Ready {
                    window,
                    observation,
                    timed_out: true,
                });
            }
            let remaining_task = self.deadline.saturating_sub(self.started.elapsed());
            if !wait_interruptible(self.host, POLL.min(remaining).min(remaining_task)) {
                return Err("Stopped".into());
            }
        }
    }
}
