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
