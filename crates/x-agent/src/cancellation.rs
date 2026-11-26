use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Stop a task and all work it owns. A child's Stop affects only that subtree.
#[derive(Clone, Default)]
pub struct Cancellation(Arc<Scope>);

#[derive(Default)]
struct Scope {
    stopped: AtomicBool,
    parent: Option<Cancellation>,
    deadline: Option<Instant>,
}

impl Cancellation {
    pub fn child(&self) -> Self {
        Self(Arc::new(Scope {
            stopped: AtomicBool::new(false),
            parent: Some(self.clone()),
            deadline: None,
        }))
    }

    pub(crate) fn until(&self, deadline: Instant) -> Self {
        Self(Arc::new(Scope {
            parent: Some(self.clone()),
            deadline: Some(deadline),
            ..Scope::default()
        }))
    }

    pub fn request(&self) -> bool {
        !self.0.stopped.swap(true, Ordering::AcqRel)
    }

    pub fn requested(&self) -> bool {
        self.0.stopped.load(Ordering::Acquire)
            || self
                .0
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            || self.0.parent.as_ref().is_some_and(Self::requested)
    }

    pub(crate) fn check(&self) -> Result<(), String> {
        if self.requested() {
            Err("Stopped".into())
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_applies_to_a_scope_and_its_children() {
        let root = Cancellation::default();
        let expired = root.until(Instant::now());
        assert!(expired.requested());
        assert!(expired.child().requested());
        assert!(!root.requested());
    }

    #[test]
    fn stop_propagates_down_to_existing_and_late_children() {
        let root = Cancellation::default();
        let a = root.child();
        let a_worker = a.child();
        let b = root.child();
        assert!(a.request());
        assert!(!a.request());
        assert!(a_worker.requested());
        assert!(!root.requested());
        assert!(!b.requested());
        root.request();
        assert!(b.requested());
        assert!(root.child().requested());
        assert!(!Cancellation::default().requested());
    }
}
