//! Exact live queued-approval review. The scheduler only exposes a request
//! after dependencies and worker availability permit dispatch.

use std::sync::Arc;
use std::time::{Duration, Instant};

use uuid::Uuid;

use super::JobStops;
use crate::grants::{Grants, Permission, Receipt, Scope};

const REVIEW_WINDOW: Duration = Duration::from_secs(10 * 60);

struct Review {
    token: Uuid,
    deadline: Instant,
}

pub(super) struct Renewal {
    grants: Arc<Grants>,
    receipt: Receipt,
    permission: Permission,
    scope: Scope,
    review: Option<Review>,
}

impl JobStops {
    pub(crate) fn register_renewal(
        &self,
        index: usize,
        grants: Arc<Grants>,
        receipt: Receipt,
        permission: Permission,
        scope: Scope,
    ) -> Result<(), String> {
        if index >= self.nodes.len() || self.requested(index) {
            return Err("The reviewed job is not available".into());
        }
        let mut renewals = self
            .renewals
            .lock()
            .map_err(|_| "Renewal state unavailable")?;
        if renewals.contains_key(&index) {
            return Err("A renewal is already registered for this job".into());
        }
        grants.enable_queue_renewal(&receipt, permission, &scope)?;
        renewals.insert(
            index,
            Renewal {
                grants,
                receipt,
                permission,
                scope,
                review: None,
            },
        );
        Ok(())
    }

    /// Some(true) means the first visible wait; Some(false) means keep waiting.
    /// A revoked or nonrenewable grant returns None and cannot be revived.
    pub(super) fn wait_for_permission(&self, index: usize) -> Option<bool> {
        if self.requested(index) {
            return None;
        }
        let mut renewals = self.renewals.lock().ok()?;
        let entry = renewals.get_mut(&index)?;
        if !entry
            .grants
            .expired_renewable(&entry.receipt, entry.permission, &entry.scope)
        {
            return None;
        }
        let now = Instant::now();
        if let Some(review) = &entry.review {
            return (now < review.deadline).then_some(false);
        }
        entry.review = Some(Review {
            token: Uuid::new_v4(),
            deadline: now.checked_add(REVIEW_WINDOW)?,
        });
        Some(true)
    }

    pub(crate) fn permission_review_token(&self, index: usize) -> Option<Uuid> {
        if self.requested(index) {
            return None;
        }
        let renewals = self.renewals.lock().ok()?;
        let entry = renewals.get(&index)?;
        let review = entry.review.as_ref()?;
        (Instant::now() < review.deadline
            && entry
                .grants
                .expired_renewable(&entry.receipt, entry.permission, &entry.scope))
        .then_some(review.token)
    }

    /// Called only by the trusted operator review handler with the exact token
    /// from a visible current task/step. It cannot issue a replacement grant.
    pub(crate) fn renew_permission(&self, index: usize, token: Uuid) -> Result<(), String> {
        let mut renewals = self
            .renewals
            .lock()
            .map_err(|_| "Renewal state unavailable")?;
        let entry = renewals
            .get_mut(&index)
            .ok_or("No matching queued permission review")?;
        let review = entry
            .review
            .as_ref()
            .ok_or("The job is not waiting for review")?;
        if self.requested(index) || token != review.token || Instant::now() >= review.deadline {
            return Err("The queued permission review changed, stopped or expired".into());
        }
        entry
            .grants
            .renew_queued(&entry.receipt, entry.permission, &entry.scope)?;
        entry.review = None;
        Ok(())
    }

    pub(super) fn close_permission_review(&self, index: usize) {
        if let Ok(mut renewals) = self.renewals.lock() {
            renewals.remove(&index);
        }
    }

    pub(super) fn close_permission_reviews(&self) {
        if let Ok(mut renewals) = self.renewals.lock() {
            renewals.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cancellation::Cancellation;

    fn fixture() -> (JobStops, Arc<Grants>, Receipt, Scope) {
        let stops = JobStops::new(2, &Cancellation::default());
        let grants = Arc::new(Grants::default());
        let scope = Scope::Task {
            project_id: Uuid::new_v4(),
            run_id: Uuid::new_v4(),
            node_id: "queued".into(),
            intent: "fixed suite".into(),
        };
        let receipt = grants
            .issue(
                Permission::QueueTests,
                scope.clone(),
                Duration::from_secs(30),
                1,
                true,
            )
            .unwrap();
        stops
            .register_renewal(
                0,
                Arc::clone(&grants),
                receipt,
                Permission::QueueTests,
                scope.clone(),
            )
            .unwrap();
        grants.expire_for_test(&receipt);
        (stops, grants, receipt, scope)
    }

    #[test]
    fn expired_receipt_has_no_authority_before_explicit_review() {
        let (stops, grants, receipt, scope) = fixture();
        assert_eq!(grants.pending_count().unwrap(), 0);
        assert!(
            grants
                .check(&receipt, Permission::QueueTests, &scope)
                .is_err()
        );
        assert!(stops.permission_review_token(0).is_none());
        assert_eq!(stops.wait_for_permission(0), Some(true));
        assert_eq!(stops.wait_for_permission(0), Some(false));
        let token = stops.permission_review_token(0).unwrap();
        assert!(stops.renew_permission(1, token).is_err());
        stops.renew_permission(0, token).unwrap();
        assert!(stops.permission_review_token(0).is_none());
        assert_eq!(grants.pending_count().unwrap(), 1);
        assert!(stops.renew_permission(0, token).is_err());
    }

    #[test]
    fn stopped_revoked_or_timed_out_wait_cannot_be_renewed() {
        let (stops, grants, receipt, _) = fixture();
        assert_eq!(stops.wait_for_permission(0), Some(true));
        let token = stops.permission_review_token(0).unwrap();
        assert!(grants.revoke(&receipt).unwrap());
        assert!(stops.renew_permission(0, token).is_err());
        assert_eq!(stops.wait_for_permission(0), None);

        let (stops, _, _, _) = fixture();
        assert_eq!(stops.wait_for_permission(0), Some(true));
        let token = stops.permission_review_token(0).unwrap();
        stops.request(0).unwrap();
        assert!(stops.renew_permission(0, token).is_err());
        assert!(!stops.requested(1));

        let (stops, _, _, _) = fixture();
        assert_eq!(stops.wait_for_permission(0), Some(true));
        let token = stops.permission_review_token(0).unwrap();
        stops
            .renewals
            .lock()
            .unwrap()
            .get_mut(&0)
            .unwrap()
            .review
            .as_mut()
            .unwrap()
            .deadline = Instant::now();
        assert!(stops.renew_permission(0, token).is_err());
        assert_eq!(stops.wait_for_permission(0), None);
    }
}
