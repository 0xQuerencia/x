//! Durable idempotency claims for approved DAG work.
//! This store never sends a task; a previously claimed send must be reconciled
//! with the provider before the owner can authorize a new attempt.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::verification::test_runner::TestEvidence;
use crate::worktrees::{CommittedArtifact, OwnedWorktree, Reservation};

const VERSION: u16 = 1;
const FILE: &str = "x-job-claims.json";
const LOCK: &str = "x-job-claims.lock";
const MAX_BYTES: usize = 1024 * 1024;
const MAX_CLAIMS: usize = 2048;
const MAX_ATTEMPTS: u8 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Claimed,
    Delivered,
    Verified,
    Failed,
    Unverified,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    pub run_id: Uuid,
    pub node_id: String,
    /// Hash of the approved, immutable task request; prompts are not stored.
    pub request_fingerprint: String,
    pub attempt: u8,
    pub state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<CommittedArtifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_evidence: Option<TestEvidence>,
    /// Durable preparation intent; historical data never restores run authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<Reservation>,
    /// Desktop execution is recorded independently of provider delivery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop: Option<DesktopClaim>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopClaim {
    /// None while interrupted/in flight. No saved desktop target restores authority.
    pub outcome: Option<super::scheduler::Outcome>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Begin {
    Fresh(Claim),
    AlreadyFinished(Claim),
    /// A crash or disconnect could have occurred after delivery.
    /// Do not resend until the provider is independently reconciled.
    RequiresReconciliation(Claim),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Data {
    version: u16,
    entries: Vec<Claim>,
}

pub struct Journal {
    directory: PathBuf,
}

impl Journal {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    /// The same run and node may be claimed only once, even across restarts.
    pub fn begin(
        &self,
        run_id: Uuid,
        node_id: &str,
        request_fingerprint: &str,
    ) -> Result<Begin, String> {
        self.begin_with_reservation(run_id, node_id, request_fingerprint, None, false)
    }

    pub(super) fn begin_desktop(
        &self,
        run: Uuid,
        node: &str,
        fingerprint: &str,
    ) -> Result<Begin, String> {
        self.begin_with_reservation(run, node, fingerprint, None, true)
    }

    /// Reserve the exact owned checkout before allocation can leave files behind.
    pub(crate) fn begin_preparation(
        &self,
        run_id: Uuid,
        node_id: &str,
        request_fingerprint: &str,
        reservation: Reservation,
    ) -> Result<Begin, String> {
        if !reservation.valid() || reservation.resolved_commit().is_some() {
            return Err("Invalid worktree preparation reservation".into());
        }
        self.begin_with_reservation(
            run_id,
            node_id,
            request_fingerprint,
            Some(reservation),
            false,
        )
    }

    fn begin_with_reservation(
        &self,
        run_id: Uuid,
        node_id: &str,
        request_fingerprint: &str,
        worktree: Option<Reservation>,
        desktop: bool,
    ) -> Result<Begin, String> {
        validate_key(run_id, node_id, request_fingerprint)?;
        self.update(|data| {
            if let Some(claim) = data
                .entries
                .iter()
                .find(|entry| entry.run_id == run_id && entry.node_id == node_id)
            {
                if claim.request_fingerprint != request_fingerprint {
                    return Err("The approved task fingerprint changed".into());
                }
                if desktop != claim.desktop.is_some() {
                    return Err("The approved job changed its execution domain".into());
                }
                if worktree.as_ref().is_some_and(|reservation| {
                    claim
                        .worktree
                        .as_ref()
                        .is_none_or(|recorded| !recorded.same_plan(reservation))
                }) {
                    return Err("The approved worktree reservation changed".into());
                }
                return Ok((
                    if claim.state == State::Claimed {
                        Begin::RequiresReconciliation(claim.clone())
                    } else {
                        Begin::AlreadyFinished(claim.clone())
                    },
                    false,
                ));
            }
            if data.entries.len() >= MAX_CLAIMS {
                return Err("The job claim journal is full".into());
            }
            let claim = Claim {
                run_id,
                node_id: node_id.into(),
                request_fingerprint: request_fingerprint.into(),
                attempt: 1,
                state: State::Claimed,
                evidence_id: None,
                artifact: None,
                test_evidence: None,
                worktree,
                desktop: desktop.then_some(DesktopClaim { outcome: None }),
            };
            data.entries.push(claim.clone());
            Ok((Begin::Fresh(claim), true))
        })
    }

    pub(super) fn settle_desktop(
        &self,
        attempt: &Claim,
        outcome: super::scheduler::Outcome,
    ) -> Result<(), String> {
        let state = desktop_state(outcome).ok_or("Invalid desktop execution outcome")?;
        self.update(|data| {
            let claim = active_attempt(data, attempt)?;
            let desktop = claim
                .desktop
                .as_mut()
                .ok_or("This is not a desktop attempt")?;
            if claim.worktree.is_some()
                || claim.evidence_id.is_some()
                || claim.test_evidence.is_some()
                || claim.artifact.is_some()
            {
                return Err("Desktop evidence cannot replace coding evidence".into());
            }
            desktop.outcome = Some(outcome);
            claim.state = state;
            Ok(((), true))
        })
    }

    /// Only a verified native checkout can complete its durable preparation.
    /// If this write fails, the checkout is retained and no job may launch.
    pub(super) fn record_checkout(
        &self,
        attempt: &Claim,
        checkout: &OwnedWorktree,
    ) -> Result<(), String> {
        if checkout.run_id() != attempt.run_id || checkout.task_id() != attempt.node_id {
            return Err("The prepared checkout belongs to a different attempt".into());
        }
        self.update(|data| {
            let claim = active_attempt(data, attempt)?;
            if claim.desktop.is_some() {
                return Err("A desktop node cannot acquire a coding checkout".into());
            }
            let reservation = claim
                .worktree
                .as_mut()
                .ok_or("This job has no preparation reservation")?;
            let changed = reservation.record_checkout(checkout)?;
            Ok(((), changed))
        })
    }

    /// Persist structured delivery evidence as soon as the harness reports it.
    /// Keep the outcome unresolved until the process and result checks finish.
    pub(super) fn record_delivery(
        &self,
        attempt: &Claim,
        session: &crate::harness::SessionId,
    ) -> Result<(), String> {
        self.update(|data| {
            let claim = active_attempt(data, attempt)?;
            if claim.desktop.is_some() {
                return Err("A desktop node cannot accept provider delivery".into());
            }
            if let Some(recorded) = &claim.evidence_id {
                if recorded != session.as_str() {
                    return Err("A different session already acknowledged this job".into());
                }
                return Ok(((), false));
            }
            claim.evidence_id = Some(session.as_str().to_owned());
            Ok(((), true))
        })
    }

    /// Settle only the specific in-flight attempt and preserve any earlier
    /// delivery evidence, even if it failed or was stopped after delivery.
    pub(super) fn settle(
        &self,
        attempt: &Claim,
        outcome: State,
        evidence_id: Option<&str>,
    ) -> Result<Claim, String> {
        self.settle_with_artifact(attempt, outcome, evidence_id, None)
    }

    /// Settle artifact evidence atomically with the matching attempt outcome.
    pub(super) fn settle_with_artifact(
        &self,
        attempt: &Claim,
        outcome: State,
        evidence_id: Option<&str>,
        artifact: Option<CommittedArtifact>,
    ) -> Result<Claim, String> {
        if artifact.as_ref().is_some_and(|artifact| {
            outcome != State::Verified || !artifact.valid_for(attempt.run_id, &attempt.node_id)
        }) {
            return Err("Artifact evidence does not belong to this verified attempt".into());
        }
        if outcome == State::Claimed {
            return Err("An in-flight claim cannot be settled as claimed".into());
        }
        if evidence_id.is_some_and(|id| !valid_evidence(id)) {
            return Err("Evidence identifier is invalid".into());
        }
        self.update(|data| {
            let claim = active_attempt(data, attempt)?;
            if claim.desktop.is_some() {
                return Err("A desktop node requires a native desktop outcome".into());
            }
            match (claim.evidence_id.as_deref(), evidence_id) {
                (Some(recorded), Some(provided)) if recorded != provided => {
                    return Err("A different session already acknowledged this job".into());
                }
                (None, Some(provided)) => claim.evidence_id = Some(provided.to_owned()),
                (None, None) if matches!(outcome, State::Delivered | State::Verified) => {
                    return Err("Verification requires a durable evidence reference".into());
                }
                _ => {}
            }
            claim.artifact = artifact;
            claim.state = outcome;
            Ok((claim.clone(), true))
        })
    }

    /// Test processes have no provider acknowledgement. Store their exact
    /// commit/suite evidence atomically with the terminal attempt instead.
    pub(super) fn settle_test(
        &self,
        attempt: &Claim,
        outcome: State,
        evidence: TestEvidence,
        artifact: Option<CommittedArtifact>,
    ) -> Result<Claim, String> {
        if !evidence.valid()
            || matches!(outcome, State::Claimed | State::Delivered)
            || (outcome == State::Verified
                && (evidence.passed != Some(true)
                    || artifact
                        .as_ref()
                        .is_none_or(|a| a.commit() != evidence.commit)))
            || artifact.as_ref().is_some_and(|a| {
                outcome != State::Verified || !a.valid_for(attempt.run_id, &attempt.node_id)
            })
        {
            return Err("Test evidence does not prove this exact attempt".into());
        }
        self.update(|data| {
            let claim = active_attempt(data, attempt)?;
            if claim.evidence_id.is_some() {
                return Err("A provider claim cannot become a test result".into());
            }
            claim.state = outcome;
            claim.artifact = artifact;
            claim.test_evidence = Some(evidence);
            Ok((claim.clone(), true))
        })
    }

    /// Retry is deliberately not automatic. A fresh owner grant and an
    /// independent check that the prior attempt wasn't delivered are required.
    pub fn approve_retry_after_reconciliation(
        &self,
        run_id: Uuid,
        node_id: &str,
        fingerprint: &str,
        owner_approved: bool,
        provider_confirms_no_delivery: bool,
    ) -> Result<Claim, String> {
        if !owner_approved || !provider_confirms_no_delivery {
            return Err("A fresh approval and proof of no prior delivery are required".into());
        }
        self.update(|data| {
            let claim = data
                .entries
                .iter_mut()
                .find(|entry| entry.run_id == run_id && entry.node_id == node_id)
                .ok_or("No job matches that run and node")?;
            if claim.request_fingerprint != fingerprint {
                return Err("The approved task fingerprint changed".into());
            }
            if claim.desktop.is_some() {
                return Err("Desktop recovery requires a new reviewed plan; provider reconciliation cannot replay desktop effects".into());
            }
            if !matches!(
                claim.state,
                State::Failed | State::Unverified | State::Cancelled
            ) {
                return Err("This job is not eligible for retry".into());
            }
            if claim.evidence_id.is_some() {
                return Err("The harness already acknowledged delivery of this job".into());
            }
            if claim.attempt >= MAX_ATTEMPTS {
                return Err("Retry budget exhausted".into());
            }
            claim.attempt += 1;
            claim.state = State::Claimed;
            claim.evidence_id = None;
            claim.artifact = None;
            claim.test_evidence = None;
            if let Some(reservation) = &mut claim.worktree {
                reservation.reset_preparation();
            }
            Ok((claim.clone(), true))
        })
    }

    /// Read every persisted claim for a local operator status view.
    /// Callers must redact fingerprints and provider evidence before display.
    /// Reading never takes a write lock, settles, replays or retries a job.
    pub fn snapshot(&self) -> Result<Vec<Claim>, String> {
        Ok(self.read()?.entries)
    }

    /// After a restart, in-flight claims are *unknown*, never silently
    /// replayed or marked successful.
    pub fn uncertain(&self) -> Result<Vec<Claim>, String> {
        Ok(self
            .read()?
            .entries
            .into_iter()
            .filter(|claim| claim.state == State::Claimed)
            .collect())
    }

    fn read(&self) -> Result<Data, String> {
        let path = self.directory.join(FILE);
        let file = match open_regular(&path, false) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Ok(Data {
                    version: VERSION,
                    entries: vec![],
                });
            }
            Err(error) => return Err(format!("Cannot read job journal: {error}")),
        };
        let mut bytes = vec![];
        file.take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("Cannot read job journal: {error}"))?;
        if bytes.len() > MAX_BYTES {
            return Err("Job journal exceeds the size limit".into());
        }
        let data: Data = serde_json::from_slice(&bytes).map_err(|_| "Cannot decode job journal")?;
        if data.version != VERSION {
            return Err("Unsupported job journal version".into());
        }
        let mut ids = HashSet::new();
        if data.entries.len() > MAX_CLAIMS
            || data.entries.iter().any(|claim| {
                validate_key(claim.run_id, &claim.node_id, &claim.request_fingerprint).is_err()
                    || claim.attempt == 0
                    || claim.attempt > MAX_ATTEMPTS
                    || claim
                        .evidence_id
                        .as_deref()
                        .is_some_and(|id| !valid_evidence(id))
                    || (matches!(claim.state, State::Delivered | State::Verified)
                        && claim.evidence_id.is_none()
                        && claim.desktop.is_none()
                        && !(claim.state == State::Verified
                            && claim.test_evidence.as_ref().is_some_and(|test| {
                                test.passed == Some(true)
                                    && claim
                                        .artifact
                                        .as_ref()
                                        .is_some_and(|a| a.commit() == test.commit)
                            })))
                    || claim.test_evidence.as_ref().is_some_and(|test| {
                        !test.valid()
                            || claim.evidence_id.is_some()
                            || matches!(claim.state, State::Claimed | State::Delivered)
                            || (claim.state == State::Verified && test.passed != Some(true))
                    })
                    || claim.artifact.as_ref().is_some_and(|artifact| {
                        claim.state != State::Verified
                            || !artifact.valid_for(claim.run_id, &claim.node_id)
                    })
                    || claim
                        .worktree
                        .as_ref()
                        .is_some_and(|worktree| !worktree.valid())
                    || claim.desktop.as_ref().is_some_and(|desktop| {
                        claim.worktree.is_some()
                            || claim.evidence_id.is_some()
                            || claim.artifact.is_some()
                            || claim.test_evidence.is_some()
                            || match desktop.outcome {
                                None => claim.state != State::Claimed,
                                Some(outcome) => desktop_state(outcome) != Some(claim.state),
                            }
                    })
                    || !ids.insert((claim.run_id, claim.node_id.as_str()))
            })
        {
            return Err("Job journal contains invalid claims".into());
        }
        Ok(data)
    }

    fn update<T>(
        &self,
        change: impl FnOnce(&mut Data) -> Result<(T, bool), String>,
    ) -> Result<T, String> {
        fs::create_dir_all(&self.directory)
            .map_err(|error| format!("Cannot create job journal directory: {error}"))?;
        let lock = open_regular(&self.directory.join(LOCK), true)
            .map_err(|error| format!("Cannot open job journal lock: {error}"))?;
        // Independent jobs can contend while a previous writer syncs both the
        // journal and its directory. Give that durable write time to finish,
        // but keep the wait bounded if the lock holder is stuck.
        let started = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(5);
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) => {
                    let remaining = timeout.saturating_sub(started.elapsed());
                    if remaining.is_zero() {
                        return Err("Another job-journal operation is in progress".into());
                    }
                    std::thread::sleep(remaining.min(std::time::Duration::from_millis(5)));
                }
                Err(std::fs::TryLockError::Error(_)) => {
                    return Err("Cannot lock job journal".into());
                }
            }
        }
        let result: Result<T, String> = (|| {
            let mut data = self.read()?;
            let (value, changed) = change(&mut data)?;
            if changed {
                let mut temp = tempfile::NamedTempFile::new_in(&self.directory)
                    .map_err(|error| format!("Cannot create job journal: {error}"))?;
                let bytes = serde_json::to_vec(&data)
                    .map_err(|error| format!("Cannot serialize job journal: {error}"))?;
                if bytes.len() > MAX_BYTES {
                    return Err("Job journal exceeds the size limit".into());
                }
                temp.write_all(&bytes)
                    .and_then(|_| temp.as_file().sync_all())
                    .map_err(|error| format!("Cannot write job journal: {error}"))?;
                temp.persist(self.directory.join(FILE))
                    .map_err(|error| format!("Cannot save job journal: {error}"))?;
                #[cfg(unix)]
                File::open(&self.directory)
                    .and_then(|dir| dir.sync_all())
                    .map_err(|error| format!("Cannot sync job journal: {error}"))?;
            }
            Ok(value)
        })();
        let unlocked = lock.unlock();
        let value = result?;
        unlocked.map_err(|_| "Cannot release job journal lock")?;
        Ok(value)
    }
}

fn desktop_state(outcome: super::scheduler::Outcome) -> Option<State> {
    use super::scheduler::Outcome;
    Some(match outcome {
        Outcome::Launched => State::Delivered,
        Outcome::Verified => State::Verified,
        Outcome::Failed => State::Failed,
        Outcome::Unverified => State::Unverified,
        Outcome::Cancelled => State::Cancelled,
        _ => return None,
    })
}

fn active_attempt<'a>(data: &'a mut Data, attempt: &Claim) -> Result<&'a mut Claim, String> {
    let claim = data
        .entries
        .iter_mut()
        .find(|entry| entry.run_id == attempt.run_id && entry.node_id == attempt.node_id)
        .ok_or("No claimed job matches that run and node")?;
    if claim.state != State::Claimed
        || claim.attempt != attempt.attempt
        || claim.request_fingerprint != attempt.request_fingerprint
    {
        return Err("This job attempt is no longer in flight".into());
    }
    Ok(claim)
}

fn valid_evidence(input: &str) -> bool {
    // Legacy evidence references used the same size limit as node IDs.
    valid_slug(input) || crate::harness::SessionId::try_from(input.to_owned()).is_ok()
}

fn validate_key(run: Uuid, node: &str, fingerprint: &str) -> Result<(), String> {
    if run.is_nil() || !valid_slug(node) {
        return Err("Invalid run or node identity".into());
    }
    if fingerprint.len() != 64 || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Task fingerprint must be a SHA-256 hex digest".into());
    }
    Ok(())
}

fn valid_slug(input: &str) -> bool {
    (1..=80).contains(&input.len())
        && input
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn open_regular(path: &Path, create: bool) -> std::io::Result<File> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => {
            return Err(std::io::Error::other("Expected a regular job journal file"));
        }
        Ok(_) => {}
        Err(error) if create && error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut opts = OpenOptions::new();
    opts.read(true).write(create).create(create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    opts.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Journal, Uuid, String) {
        let directory = tempfile::tempdir().unwrap();
        let journal = Journal::new(directory.path().join("state"));
        (directory, journal, Uuid::new_v4(), "a".repeat(64))
    }

    fn begin(journal: &Journal, run_id: Uuid, node: &str, fingerprint: &str) -> Claim {
        let Begin::Fresh(claim) = journal.begin(run_id, node, fingerprint).unwrap() else {
            panic!("expected a fresh claim");
        };
        claim
    }

    #[test]
    fn a_contending_job_waits_for_a_slow_journal_writer() {
        let (_directory, journal, run, digest) = fixture();
        fs::create_dir_all(&journal.directory).unwrap();
        let lock = open_regular(&journal.directory.join(LOCK), true).unwrap();
        lock.try_lock().unwrap();

        std::thread::scope(|scope| {
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let journal = &journal;
            let digest = &digest;
            let worker = scope.spawn(move || {
                started_tx.send(()).unwrap();
                journal.begin(run, "backend", digest)
            });
            started_rx.recv().unwrap();
            // Exceed the previous 100 ms retry window without changing the
            // journal or allowing the waiting job to claim it prematurely.
            std::thread::sleep(std::time::Duration::from_millis(300));
            let finished_early = worker.is_finished();
            let before_release = journal.snapshot();
            lock.unlock().unwrap();

            let outcome = worker.join().unwrap().unwrap();
            assert!(!finished_early);
            assert!(before_release.unwrap().is_empty());
            assert!(matches!(outcome, Begin::Fresh(_)));
        });
        assert_eq!(journal.snapshot().unwrap().len(), 1);
    }

    #[test]
    fn a_crashed_claim_is_never_dispatched_twice() {
        let (directory, journal, run, digest) = fixture();
        let first = journal.begin(run, "backend", &digest).unwrap();
        assert!(matches!(first, Begin::Fresh(_)));
        let reopened = Journal::new(directory.path().join("state"));
        assert_eq!(reopened.uncertain().unwrap().len(), 1);
        assert!(matches!(
            reopened.begin(run, "backend", &digest).unwrap(),
            Begin::RequiresReconciliation(_)
        ));
        assert!(reopened.begin(run, "backend", &"b".repeat(64)).is_err());
    }

    #[test]
    fn finished_jobs_never_get_replayed_without_explicit_reconciliation() {
        let (_directory, journal, run, digest) = fixture();
        let claim = begin(&journal, run, "backend", &digest);
        assert!(journal.settle(&claim, State::Verified, None).is_err());
        let finished = journal
            .settle(&claim, State::Verified, Some("harness_event_7"))
            .unwrap();
        assert_eq!(finished.state, State::Verified);
        assert!(matches!(
            journal.begin(run, "backend", &digest).unwrap(),
            Begin::AlreadyFinished(_)
        ));
        assert!(
            journal
                .approve_retry_after_reconciliation(run, "backend", &digest, true, true)
                .is_err()
        );
    }

    #[test]
    fn retries_need_fresh_owner_approval_proof_and_budget() {
        let (_directory, journal, run, digest) = fixture();
        let first = begin(&journal, run, "frontend", &digest);
        journal.settle(&first, State::Failed, None).unwrap();
        assert!(
            journal
                .approve_retry_after_reconciliation(run, "frontend", &digest, false, true)
                .is_err()
        );
        assert!(
            journal
                .approve_retry_after_reconciliation(run, "frontend", &digest, true, false)
                .is_err()
        );
        for _ in 0..2 {
            let retry = journal
                .approve_retry_after_reconciliation(run, "frontend", &digest, true, true)
                .unwrap();
            assert!(journal.settle(&first, State::Failed, None).is_err());
            journal.settle(&retry, State::Failed, None).unwrap();
        }
        assert!(
            journal
                .approve_retry_after_reconciliation(run, "frontend", &digest, true, true)
                .is_err()
        );
        assert!(journal.uncertain().unwrap().is_empty());
    }

    #[test]
    fn delivery_survives_reopening_the_journal_and_blocks_replay_after_failure() {
        use crate::harness::SessionId;

        let (directory, journal, run, digest) = fixture();
        let claim = begin(&journal, run, "backend", &digest);
        let session = SessionId::try_from(format!("ses_{}", "a".repeat(120))).unwrap();
        let other = SessionId::try_from(String::from("ses_other")).unwrap();
        journal.record_delivery(&claim, &session).unwrap();
        journal.record_delivery(&claim, &session).unwrap();
        assert!(journal.record_delivery(&claim, &other).is_err());

        let reopened = Journal::new(directory.path().join("state"));
        let recovered = reopened.uncertain().unwrap();
        assert_eq!(recovered[0].evidence_id.as_deref(), Some(session.as_str()));
        assert!(matches!(
            reopened.begin(run, "backend", &digest).unwrap(),
            Begin::RequiresReconciliation(_)
        ));
        assert!(
            reopened
                .settle(&claim, State::Verified, Some(other.as_str()))
                .is_err()
        );
        let failed = reopened.settle(&claim, State::Failed, None).unwrap();
        assert_eq!(failed.evidence_id.as_deref(), Some(session.as_str()));
        assert!(
            reopened
                .approve_retry_after_reconciliation(run, "backend", &digest, true, true)
                .is_err()
        );
    }

    #[test]
    fn unknown_versions_are_preserved_without_overwriting() {
        let (directory, journal, run, digest) = fixture();
        let path = directory.path().join("state").join(FILE);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let existing = br#"{"version":99,"entries":[]}"#;
        fs::write(&path, existing).unwrap();
        assert!(journal.begin(run, "backend", &digest).is_err());
        assert_eq!(fs::read(&path).unwrap(), existing);
    }

    #[test]
    fn snapshot_does_not_replay_or_mutate_claims() {
        let (_dir, journal, run, digest) = fixture();
        let claim = begin(&journal, run, "backend", &digest);
        let saved = journal.snapshot().unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].state, State::Claimed);
        assert_eq!(saved[0].attempt, 1);
        assert!(matches!(
            journal.begin(run, "backend", &digest).unwrap(),
            Begin::RequiresReconciliation(_)
        ));
        journal.settle(&claim, State::Failed, None).unwrap();
        let after = journal.snapshot().unwrap();
        assert_eq!(after[0].state, State::Failed);
        assert_eq!(after[0].attempt, 1);
        assert!(journal.uncertain().unwrap().is_empty());
    }

    #[test]
    fn duplicate_inflight_claims_stay_single_entry() {
        let (_directory, journal, run, digest) = fixture();
        journal.begin(run, "backend", &digest).unwrap();
        for _ in 0..4 {
            assert!(matches!(
                journal.begin(run, "backend", &digest).unwrap(),
                Begin::RequiresReconciliation(_)
            ));
        }
        assert_eq!(journal.uncertain().unwrap().len(), 1);
    }

    #[test]
    fn desktop_claims_preserve_uncertainty_and_cannot_use_provider_retry_authority() {
        use super::super::scheduler::Outcome;
        let (directory, journal, run, digest) = fixture();
        let Begin::Fresh(claim) = journal.begin_desktop(run, "focus", &digest).unwrap() else {
            panic!("expected fresh desktop claim");
        };
        let reopened = Journal::new(directory.path().join("state"));
        assert!(matches!(
            reopened.begin_desktop(run, "focus", &digest).unwrap(),
            Begin::RequiresReconciliation(_)
        ));
        assert!(reopened.begin(run, "focus", &digest).is_err());
        assert!(
            reopened.snapshot().unwrap()[0]
                .desktop
                .as_ref()
                .unwrap()
                .outcome
                .is_none()
        );
        let session =
            crate::harness::SessionId::try_from(String::from("ses_not_desktop_proof")).unwrap();
        assert!(reopened.record_delivery(&claim, &session).is_err());
        assert!(
            reopened
                .settle(&claim, State::Verified, Some(session.as_str()))
                .is_err()
        );
        assert!(reopened.settle_desktop(&claim, Outcome::Delivered).is_err());
        reopened.settle_desktop(&claim, Outcome::Failed).unwrap();
        assert!(
            reopened
                .approve_retry_after_reconciliation(run, "focus", &digest, true, true)
                .is_err()
        );
        assert_eq!(
            reopened.snapshot().unwrap()[0]
                .desktop
                .as_ref()
                .unwrap()
                .outcome,
            Some(Outcome::Failed)
        );
        let Begin::Fresh(second) = reopened.begin_desktop(run, "page", &digest).unwrap() else {
            panic!("expected fresh page claim");
        };
        reopened.settle_desktop(&second, Outcome::Launched).unwrap();
        assert_eq!(
            Journal::new(directory.path().join("state"))
                .snapshot()
                .unwrap()[1]
                .state,
            State::Delivered
        );
    }

    #[test]
    fn preparation_identity_survives_restart_and_cannot_change_under_the_same_claim() {
        let (directory, journal, run, digest) = fixture();
        let mut project = crate::projects::Project {
            id: Uuid::new_v4(),
            name: "Demo".into(),
            aliases: vec![],
            root: directory.path().join("repo"),
            git_common_dir: Some(directory.path().join("repo/.git")),
        };
        let base = "a".repeat(40);
        let reservation = Reservation::planned(&project, &base).unwrap();
        let Begin::Fresh(first) = journal
            .begin_preparation(run, "backend", &digest, reservation.clone())
            .unwrap()
        else {
            panic!("expected a fresh preparation claim");
        };
        let reopened = Journal::new(directory.path().join("state"));
        assert_eq!(reopened.uncertain().unwrap(), vec![first]);
        assert!(matches!(
            reopened
                .begin_preparation(run, "backend", &digest, reservation)
                .unwrap(),
            Begin::RequiresReconciliation(_)
        ));
        assert!(
            reopened
                .begin_preparation(
                    run,
                    "backend",
                    &digest,
                    Reservation::planned(&project, &"b".repeat(40)).unwrap(),
                )
                .is_err()
        );
        project.root = directory.path().join("other");
        assert!(
            reopened
                .begin_preparation(
                    run,
                    "backend",
                    &digest,
                    Reservation::planned(&project, &base).unwrap(),
                )
                .is_err()
        );

        let path = directory.path().join("state").join(FILE);
        let mut invalid: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        invalid["entries"][0]["worktree"]["approved_base"] = "main".into();
        let bytes = serde_json::to_vec(&invalid).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert!(reopened.snapshot().is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }

    #[cfg(unix)]
    #[test]
    fn prepared_checkout_records_cannot_cross_nodes_or_outlive_their_attempt() {
        use crate::projects::Registry;
        use crate::worktrees::Manager;

        let (directory, journal, run, digest) = fixture();
        let home = directory.path().join("home");
        let root = home.join("project");
        let base = home.join("Worktrees");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir(&base).unwrap();
        for args in [
            vec!["init", "-q"],
            vec![
                "-c",
                "user.name=X",
                "-c",
                "user.email=x@localhost",
                "-c",
                "core.hooksPath=",
                "-c",
                "commit.gpgSign=false",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "fixture",
            ],
        ] {
            assert!(
                std::process::Command::new("git")
                    .current_dir(&root)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let (project, _) = Registry::new(home.join("state"), home.clone())
            .register("Demo", root.to_str().unwrap())
            .unwrap();
        let manager = Manager::new(base, home);
        let sha = manager.starting_commit(&project).unwrap();
        let Begin::Fresh(first) = journal
            .begin_preparation(
                run,
                "first",
                &digest,
                Reservation::planned(&project, &sha).unwrap(),
            )
            .unwrap()
        else {
            panic!("expected a fresh claim");
        };
        let tree = manager
            .allocate(&project, run, "first", &sha, true)
            .unwrap();
        let other = manager
            .allocate(&project, run, "other", &sha, true)
            .unwrap();
        assert!(journal.record_checkout(&first, &other).is_err());
        journal.record_checkout(&first, &tree).unwrap();
        let recorded = journal.snapshot().unwrap();
        assert_eq!(
            recorded[0].worktree.as_ref().unwrap().resolved_commit(),
            Some(sha.as_str())
        );
        journal.settle(&first, State::Failed, None).unwrap();
        assert!(journal.record_checkout(&first, &tree).is_err());
        let retry = journal
            .approve_retry_after_reconciliation(run, "first", &digest, true, true)
            .unwrap();
        assert_eq!(retry.attempt, 2);
        assert_eq!(retry.worktree.as_ref().unwrap().resolved_commit(), None);
        assert!(journal.record_checkout(&first, &tree).is_err());
        journal.record_checkout(&retry, &tree).unwrap();
        assert!(tree.path().is_dir());
    }
}
