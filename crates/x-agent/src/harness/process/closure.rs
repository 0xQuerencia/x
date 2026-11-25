//! An expired termination wait is progress, never permission to drop ownership.

use std::thread;
use std::time::{Duration, Instant};

pub(super) enum State {
    Empty,
    Active,
    Unavailable,
}

pub(super) fn wait_until_empty(
    grace: Duration,
    mut poll: impl FnMut() -> State,
    on_unconfirmed: impl FnOnce(),
) -> Duration {
    let start = Instant::now();
    let mut notify = Some(on_unconfirmed);
    loop {
        if matches!(poll(), State::Empty) {
            return start.elapsed();
        }
        if start.elapsed() >= grace
            && let Some(notify) = notify.take()
        {
            notify();
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn expired_or_unavailable_closure_retains_owner_until_independent_empty_readback() {
        struct Lease(Arc<AtomicBool>);
        impl Drop for Lease {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let held = Arc::new(AtomicBool::new(true));
        let state = Arc::new(AtomicUsize::new(0));
        let (reported, rx) = mpsc::channel();
        let lease = Lease(Arc::clone(&held));
        let current = Arc::clone(&state);
        let worker = thread::spawn(move || {
            let _lease = lease;
            wait_until_empty(
                Duration::from_millis(1),
                || match current.load(Ordering::SeqCst) {
                    0 => State::Active,
                    1 => State::Unavailable,
                    _ => State::Empty,
                },
                || reported.send(()).unwrap(),
            );
        });
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(held.load(Ordering::SeqCst));
        assert!(!worker.is_finished());
        state.store(1, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(60));
        assert!(held.load(Ordering::SeqCst));
        assert!(!worker.is_finished());
        assert!(rx.try_recv().is_err());
        state.store(2, Ordering::SeqCst);
        worker.join().unwrap();
        assert!(!held.load(Ordering::SeqCst));
    }

    #[test]
    fn independently_empty_job_needs_no_timeout_warning() {
        wait_until_empty(Duration::ZERO, || State::Empty, || panic!("already empty"));
    }
}
