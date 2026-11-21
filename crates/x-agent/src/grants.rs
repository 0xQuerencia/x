//! One-process, explicit, task-scoped grants.
//! No model output or imported workflow can mint these. A grant never approves
//! merges, deletion, secret access or network transfers.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Permission {
    /// Read local project metadata; no file contents are implied.
    InspectProject,
    /// Start one approved, owned coding-harness task.
    StartHarness,
    /// Run one approved test suite in an owned Git worktree.
    RunTests,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum Scope {
    Project {
        project_id: Uuid,
    },
    Task {
        project_id: Uuid,
        run_id: Uuid,
        node_id: String,
        /// Exact canonical task request, including its verifier profile.
        intent: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct Receipt {
    id: Uuid,
}

struct Grant {
    scope: Scope,
    permission: Permission,
    expires_at: Instant,
    remaining: u8,
}

/// No persistence: restarting X always discards delegated authorization.
/// Checking and consuming a receipt are atomic, preventing double-use races.
#[derive(Default)]
pub struct Grants {
    grants: Mutex<HashMap<Uuid, Grant>>,
}

impl Grants {
    /// Only the trusted native approval handler should issue a receipt.
    /// `owner_approved` must reflect a real, current user interaction and is
    /// intentionally inaccessible to downstream adapters and model output.
    pub(crate) fn issue(
        &self,
        permission: Permission,
        scope: Scope,
        duration: Duration,
        uses: u8,
        owner_approved: bool,
    ) -> Result<Receipt, String> {
        if !owner_approved {
            return Err("Owner approval is required".into());
        }
        let limit = match permission {
            Permission::InspectProject => (Duration::from_secs(600), 16),
            Permission::StartHarness | Permission::RunTests => (Duration::from_secs(60), 1),
        };
        if duration.is_zero() || duration > limit.0 || uses == 0 || uses > limit.1 {
            return Err("Grant exceeds its permitted lifetime or use count".into());
        }
        match (&permission, &scope) {
            (Permission::InspectProject, Scope::Project { project_id }) if !project_id.is_nil() => {
            }
            (
                Permission::StartHarness | Permission::RunTests,
                Scope::Task {
                    project_id,
                    run_id,
                    node_id,
                    intent,
                },
            ) if !project_id.is_nil()
                && !run_id.is_nil()
                && valid_node_id(node_id)
                && !intent.is_empty()
                && intent.len() <= 20_000 => {}
            _ => return Err("Grant scope does not match its operation".into()),
        }
        let expires_at = Instant::now()
            .checked_add(duration)
            .ok_or("Grant lifetime is invalid")?;
        let id = Uuid::new_v4();
        let mut guard = self
            .grants
            .lock()
            .map_err(|_| "Grant storage is unavailable")?;
        if guard.len() >= 256 {
            guard.retain(|_, entry| entry.remaining > 0 && Instant::now() < entry.expires_at);
        }
        if guard.len() >= 256 {
            return Err("Too many outstanding approvals".into());
        }
        guard.insert(
            id,
            Grant {
                scope,
                permission,
                expires_at,
                remaining: uses,
            },
        );
        Ok(Receipt { id })
    }

    /// Checks an exact scope at preflight without spending the approval.
    /// Consumption must still happen immediately before an action starts.
    pub fn check(
        &self,
        receipt: &Receipt,
        permission: Permission,
        scope: &Scope,
    ) -> Result<(), String> {
        let guard = self
            .grants
            .lock()
            .map_err(|_| "Grant storage is unavailable")?;
        let grant = guard
            .get(&receipt.id)
            .ok_or("Approval is missing, exhausted or revoked")?;
        if Instant::now() >= grant.expires_at {
            return Err("Approval has expired".into());
        }
        if grant.remaining == 0 || grant.permission != permission || &grant.scope != scope {
            return Err("Approval does not cover this target or operation".into());
        }
        Ok(())
    }

    /// Consumes one use only for an exact permission and scope match.
    /// A failed check cannot decrement or broaden the grant.
    pub fn consume(
        &self,
        receipt: &Receipt,
        permission: Permission,
        scope: &Scope,
    ) -> Result<(), String> {
        self.consume_at(receipt, permission, scope, Instant::now())
    }

    fn consume_at(
        &self,
        receipt: &Receipt,
        permission: Permission,
        scope: &Scope,
        now: Instant,
    ) -> Result<(), String> {
        let mut guard = self
            .grants
            .lock()
            .map_err(|_| "Grant storage is unavailable")?;
        let grant = guard
            .get_mut(&receipt.id)
            .ok_or("Approval is missing, exhausted or revoked")?;
        if now >= grant.expires_at {
            guard.remove(&receipt.id);
            return Err("Approval has expired".into());
        }
        if grant.permission != permission || &grant.scope != scope {
            return Err("Approval does not cover this target or operation".into());
        }
        grant.remaining -= 1;
        if grant.remaining == 0 {
            guard.remove(&receipt.id);
        }
        Ok(())
    }

    /// Count unexpired, unspent receipts without revealing grant IDs or
    /// task intent. Expired entries are evicted during the read.
    pub fn pending_count(&self) -> Result<usize, String> {
        let mut guard = self
            .grants
            .lock()
            .map_err(|_| "Grant storage is unavailable")?;
        let now = Instant::now();
        guard.retain(|_, grant| grant.remaining > 0 && grant.expires_at > now);
        Ok(guard.len())
    }

    /// Clear all pending receipts when a workflow is stopped or its owner
    /// leaves the approval surface. Running child processes need their own
    /// cancellation token; clearing a receipt only blocks future dispatches.
    pub fn revoke_all(&self) -> Result<usize, String> {
        let mut entries = self
            .grants
            .lock()
            .map_err(|_| "Grant storage is unavailable")?;
        let count = entries.len();
        entries.clear();
        Ok(count)
    }

    /// Revoke only approvals tied to one registered project. Other projects
    /// keep their own pending receipts.
    pub fn revoke_project(&self, project_id: Uuid) -> Result<usize, String> {
        self.revoke_where(|scope| match scope {
            Scope::Project { project_id: id } | Scope::Task { project_id: id, .. } => {
                *id == project_id
            }
        })
    }

    /// Stop a single workflow run without invalidating other runs or a
    /// project's read-only inspection receipts.
    pub fn revoke_run(&self, project_id: Uuid, run_id: Uuid) -> Result<usize, String> {
        self.revoke_where(|scope| {
            matches!(
                scope,
                Scope::Task {
                    project_id: id,
                    run_id: run,
                    ..
                } if *id == project_id && *run == run_id
            )
        })
    }

    fn revoke_where(&self, predicate: impl Fn(&Scope) -> bool) -> Result<usize, String> {
        let mut guard = self
            .grants
            .lock()
            .map_err(|_| "Grant storage is unavailable")?;
        let before = guard.len();
        guard.retain(|_, grant| !predicate(&grant.scope));
        Ok(before - guard.len())
    }

    /// Revocation takes effect before any new consumption, even in Autonomous mode.
    pub fn revoke(&self, receipt: &Receipt) -> Result<bool, String> {
        self.grants
            .lock()
            .map(|mut entries| entries.remove(&receipt.id).is_some())
            .map_err(|_| "Grant storage is unavailable".into())
    }
}

fn valid_node_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 40
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
        && id.as_bytes()[0].is_ascii_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task() -> Scope {
        Scope::Task {
            project_id: Uuid::new_v4(),
            run_id: Uuid::new_v4(),
            node_id: "backend".into(),
            intent: "canonical-task-intent".into(),
        }
    }

    #[test]
    fn task_receipts_cannot_cross_projects_or_capabilities() {
        let grants = Grants::default();
        let target = task();
        let receipt = grants
            .issue(
                Permission::StartHarness,
                target.clone(),
                Duration::from_secs(20),
                1,
                true,
            )
            .unwrap();
        assert!(
            grants
                .consume(&receipt, Permission::StartHarness, &task())
                .is_err()
        );
        assert!(
            grants
                .consume(&receipt, Permission::RunTests, &target)
                .is_err()
        );
        let mut different_goal = target.clone();
        if let Scope::Task { intent, .. } = &mut different_goal {
            *intent = "an unauthorized replacement request".into();
        }
        assert!(
            grants
                .consume(&receipt, Permission::StartHarness, &different_goal)
                .is_err()
        );
        grants
            .consume(&receipt, Permission::StartHarness, &target)
            .unwrap();
        assert!(
            grants
                .consume(&receipt, Permission::StartHarness, &target)
                .is_err()
        );
    }

    #[test]
    fn no_approval_no_lease_and_no_broad_workflow_task_grants() {
        let grants = Grants::default();
        let target = task();
        assert!(
            grants
                .issue(
                    Permission::StartHarness,
                    target.clone(),
                    Duration::from_secs(20),
                    1,
                    false
                )
                .is_err()
        );
        assert!(
            grants
                .issue(
                    Permission::StartHarness,
                    target.clone(),
                    Duration::from_secs(20),
                    2,
                    true
                )
                .is_err()
        );
        assert!(
            grants
                .issue(
                    Permission::StartHarness,
                    target.clone(),
                    Duration::from_secs(61),
                    1,
                    true
                )
                .is_err()
        );
        assert!(
            grants
                .issue(
                    Permission::RunTests,
                    Scope::Project {
                        project_id: Uuid::new_v4()
                    },
                    Duration::from_secs(10),
                    1,
                    true
                )
                .is_err()
        );
        assert!(
            grants
                .issue(
                    Permission::InspectProject,
                    target,
                    Duration::from_secs(5),
                    1,
                    true
                )
                .is_err()
        );
    }

    #[test]
    fn expiration_and_revocation_block_consumption() {
        let grants = Grants::default();
        let target = task();
        let receipt = grants
            .issue(
                Permission::RunTests,
                target.clone(),
                Duration::from_millis(20),
                1,
                true,
            )
            .unwrap();
        let late = Instant::now() + Duration::from_secs(1);
        assert!(
            grants
                .consume_at(&receipt, Permission::RunTests, &target, late)
                .is_err()
        );
        let receipt = grants
            .issue(
                Permission::RunTests,
                target.clone(),
                Duration::from_secs(20),
                1,
                true,
            )
            .unwrap();
        assert!(grants.revoke(&receipt).unwrap());
        assert!(!grants.revoke(&receipt).unwrap());
        assert!(
            grants
                .consume(&receipt, Permission::RunTests, &target)
                .is_err()
        );
    }

    #[test]
    fn one_use_receipt_cannot_be_consumed_by_parallel_workers() {
        let grants = std::sync::Arc::new(Grants::default());
        let scope = task();
        let receipt = grants
            .issue(
                Permission::StartHarness,
                scope.clone(),
                Duration::from_secs(20),
                1,
                true,
            )
            .unwrap();
        let handles = (0..8)
            .map(|_| {
                let grants = std::sync::Arc::clone(&grants);
                let scope = scope.clone();
                std::thread::spawn(move || {
                    grants
                        .consume(&receipt, Permission::StartHarness, &scope)
                        .is_ok()
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .filter(|won| *won)
                .count(),
            1
        );
    }

    #[test]
    fn global_revoke_blocks_all_outstanding_task_receipts() {
        let grants = Grants::default();
        let first = task();
        let second = task();
        let a = grants
            .issue(
                Permission::RunTests,
                first.clone(),
                Duration::from_secs(20),
                1,
                true,
            )
            .unwrap();
        let b = grants
            .issue(
                Permission::StartHarness,
                second.clone(),
                Duration::from_secs(20),
                1,
                true,
            )
            .unwrap();
        assert_eq!(grants.revoke_all().unwrap(), 2);
        assert!(grants.consume(&a, Permission::RunTests, &first).is_err());
        assert!(
            grants
                .consume(&b, Permission::StartHarness, &second)
                .is_err()
        );
        assert_eq!(grants.revoke_all().unwrap(), 0);
    }

    #[test]
    fn metadata_grants_cannot_authorize_harness_or_tests() {
        let grants = Grants::default();
        let project_id = Uuid::new_v4();
        let scope = Scope::Project { project_id };
        let receipt = grants
            .issue(
                Permission::InspectProject,
                scope.clone(),
                Duration::from_secs(60),
                2,
                true,
            )
            .unwrap();
        assert!(
            grants
                .consume(&receipt, Permission::StartHarness, &task())
                .is_err()
        );
        grants
            .consume(&receipt, Permission::InspectProject, &scope)
            .unwrap();
        grants
            .consume(&receipt, Permission::InspectProject, &scope)
            .unwrap();
        assert!(
            grants
                .consume(&receipt, Permission::InspectProject, &scope)
                .is_err()
        );
    }
    #[test]
    fn project_revocation_does_not_cancel_other_projects() {
        let grants = Grants::default();
        let first = task();
        let second = task();
        let Scope::Task {
            project_id: project_a,
            ..
        } = &first
        else {
            unreachable!()
        };
        let a = grants
            .issue(
                Permission::StartHarness,
                first.clone(),
                Duration::from_secs(20),
                1,
                true,
            )
            .unwrap();
        let b = grants
            .issue(
                Permission::RunTests,
                second.clone(),
                Duration::from_secs(20),
                1,
                true,
            )
            .unwrap();
        assert_eq!(grants.revoke_project(*project_a).unwrap(), 1);
        assert!(
            grants
                .consume(&a, Permission::StartHarness, &first)
                .is_err()
        );
        grants.consume(&b, Permission::RunTests, &second).unwrap();
        assert_eq!(grants.revoke_project(*project_a).unwrap(), 0);
    }

    #[test]
    fn run_revocation_preserves_other_runs_and_project_inspection() {
        let grants = Grants::default();
        let first = task();
        let Scope::Task {
            project_id, run_id, ..
        } = &first
        else {
            unreachable!()
        };
        let (project_id, run_id) = (*project_id, *run_id);
        let mut other = first.clone();
        if let Scope::Task { run_id, .. } = &mut other {
            *run_id = Uuid::new_v4();
        }
        let inspection = Scope::Project { project_id };
        let a = grants
            .issue(
                Permission::StartHarness,
                first.clone(),
                Duration::from_secs(20),
                1,
                true,
            )
            .unwrap();
        let b = grants
            .issue(
                Permission::RunTests,
                other.clone(),
                Duration::from_secs(20),
                1,
                true,
            )
            .unwrap();
        let c = grants
            .issue(
                Permission::InspectProject,
                inspection.clone(),
                Duration::from_secs(20),
                1,
                true,
            )
            .unwrap();
        assert_eq!(grants.revoke_run(project_id, run_id).unwrap(), 1);
        assert!(
            grants
                .consume(&a, Permission::StartHarness, &first)
                .is_err()
        );
        grants.consume(&b, Permission::RunTests, &other).unwrap();
        grants
            .consume(&c, Permission::InspectProject, &inspection)
            .unwrap();
    }
    #[test]
    fn pending_count_omits_expired_and_spent_receipts() {
        let grants = Grants::default();
        let scope = task();
        let expired = grants
            .issue(
                Permission::RunTests,
                scope.clone(),
                Duration::from_millis(1),
                1,
                true,
            )
            .unwrap();
        let live = grants
            .issue(
                Permission::StartHarness,
                scope.clone(),
                Duration::from_secs(30),
                1,
                true,
            )
            .unwrap();
        std::thread::sleep(Duration::from_millis(10));
        assert_eq!(grants.pending_count().unwrap(), 1);
        assert!(
            grants
                .consume(&expired, Permission::RunTests, &scope)
                .is_err()
        );
        grants
            .consume(&live, Permission::StartHarness, &scope)
            .unwrap();
        assert_eq!(grants.pending_count().unwrap(), 0);
    }
}
