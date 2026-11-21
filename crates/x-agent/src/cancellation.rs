use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Stop a task and all work it owns. A child's Stop affects only that subtree.
#[derive(Clone, Default)]
pub struct Cancellation(Arc<Scope>);

#[derive(Default)]
struct Scope {
    stopped: AtomicBool,
    parent: Option<Cancellation>,
}

impl Cancellation {
    pub fn child(&self) -> Self {
        Self(Arc::new(Scope {
            stopped: AtomicBool::new(false),
            parent: Some(self.clone()),
        }))
    }

    pub fn request(&self) -> bool {
        !self.0.stopped.swap(true, Ordering::AcqRel)
    }

    pub fn requested(&self) -> bool {
        self.0.stopped.load(Ordering::Acquire)
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
