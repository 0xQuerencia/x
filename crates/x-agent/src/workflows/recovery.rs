//! Durable idempotency claims for approved DAG work.
//! This store never sends a task; a previously claimed send must be reconciled
//! with the provider before the owner can authorize a new attempt.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
        validate_key(run_id, node_id, request_fingerprint)?;
        self.update(|data| {
            if let Some(claim) = data.entries.iter().find(|entry| {
                entry.run_id == run_id && entry.node_id == node_id
            }) {
                if claim.request_fingerprint != request_fingerprint {
                    return Err("The approved task fingerprint changed".into());
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
            };
            data.entries.push(claim.clone());
            Ok((Begin::Fresh(claim), true))
        })
    }

    /// Call only after a verified provider/event observation; this method
    /// deliberately does not infer success from a process exit or model prose.
    pub fn settle(
        &self,
        run_id: Uuid,
        node_id: &str,
        outcome: State,
        evidence_id: Option<&str>,
    ) -> Result<Claim, String> {
        if outcome == State::Claimed {
            return Err("An in-flight claim cannot be settled as claimed".into());
        }
        let evidence_id = match evidence_id {
            Some(id) if valid_slug(id) => Some(id.to_owned()),
            Some(_) => return Err("Evidence identifier is invalid".into()),
            None => None,
        };
        if outcome == State::Verified && evidence_id.is_none() {
            return Err("Verification requires a durable evidence reference".into());
        }
        self.update(|data| {
            let claim = data
                .entries
                .iter_mut()
                .find(|entry| entry.run_id == run_id && entry.node_id == node_id)
                .ok_or("No claimed job matches that run and node")?;
            if claim.state != State::Claimed {
                return Err("The job was already settled; do not overwrite its result".into());
            }
            claim.state = outcome;
            claim.evidence_id = evidence_id;
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
            if !matches!(claim.state, State::Failed | State::Unverified | State::Cancelled) {
                return Err("This job is not eligible for retry".into());
            }
            if claim.attempt >= MAX_ATTEMPTS {
                return Err("Retry budget exhausted".into());
            }
            claim.attempt += 1;
            claim.state = State::Claimed;
            claim.evidence_id = None;
            Ok((claim.clone(), true))
        })
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
        let data: Data =
            serde_json::from_slice(&bytes).map_err(|_| "Cannot decode job journal")?;
        if data.version != VERSION {
            return Err("Unsupported job journal version".into());
        }
        let mut ids = HashSet::new();
        if data.entries.len() > MAX_CLAIMS
            || data.entries.iter().any(|claim| {
                validate_key(claim.run_id, &claim.node_id, &claim.request_fingerprint).is_err()
                    || claim.attempt == 0
                    || claim.attempt > MAX_ATTEMPTS
                    || claim.evidence_id.as_deref().is_some_and(|id| !valid_slug(id))
                    || (claim.state == State::Verified && claim.evidence_id.is_none())
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
        lock.try_lock()
            .map_err(|_| "Another job-journal operation is in progress")?;
        let result = (|| {
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
        journal.begin(run, "backend", &digest).unwrap();
        assert!(journal.settle(run, "backend", State::Verified, None).is_err());
        let finished = journal
            .settle(run, "backend", State::Verified, Some("harness_event_7"))
            .unwrap();
        assert_eq!(finished.state, State::Verified);
        assert!(matches!(
            journal.begin(run, "backend", &digest).unwrap(),
            Begin::AlreadyFinished(_)
        ));
        assert!(journal
            .approve_retry_after_reconciliation(run, "backend", &digest, true, true)
            .is_err());
    }

    #[test]
    fn retries_need_fresh_owner_approval_proof_and_budget() {
        let (_directory, journal, run, digest) = fixture();
        journal.begin(run, "frontend", &digest).unwrap();
        journal.settle(run, "frontend", State::Failed, None).unwrap();
        assert!(journal
            .approve_retry_after_reconciliation(run, "frontend", &digest, false, true)
            .is_err());
        assert!(journal
            .approve_retry_after_reconciliation(run, "frontend", &digest, true, false)
            .is_err());
        for _ in 0..2 {
            journal
                .approve_retry_after_reconciliation(run, "frontend", &digest, true, true)
                .unwrap();
            journal.settle(run, "frontend", State::Failed, None).unwrap();
        }
        assert!(journal
            .approve_retry_after_reconciliation(run, "frontend", &digest, true, true)
            .is_err());
        assert!(journal.uncertain().unwrap().is_empty());
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
}
