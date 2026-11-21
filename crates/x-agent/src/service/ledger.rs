use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::{Mode, Phase, RequestMetadata, now};

const FILE: &str = "x-action-ledger.ndjson";
const LEGACY: &str = "x-approval-audit.ndjson";
const HEADER: &[u8] = b"{\"schema\":1}\n";
const MAX_RECORD: usize = 4096;

pub(super) struct Ledger {
    path: PathBuf,
    instance: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Kind {
    Planned,
    Approved,
    Automatic,
    Discarded,
    StopRequested,
    Claimed,
    StepDone,
    Launched,
    StepFailed,
    Finished,
}

#[derive(Serialize)]
struct Record<'a> {
    offset: u64,
    timestamp: u64,
    instance: &'a str,
    task: u64,
    phase: Phase,
    mode: Mode,
    kind: Kind,
    #[serde(skip_serializing_if = "Option::is_none")]
    step: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    action: Option<&'a RequestMetadata>,
}

#[derive(Deserialize)]
struct LegacyEvent {
    timestamp: u64,
    event: String,
}

impl Ledger {
    pub(super) fn open(directory: &Path) -> io::Result<Self> {
        let path = directory.join(FILE);
        if !path.try_exists()? {
            initialize(directory, &path)?;
        }
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let ledger = Self {
            path,
            instance: format!("{}-{stamp}", std::process::id()),
        };
        ledger.open_for_append()?;
        Ok(ledger)
    }

    pub(super) fn record(
        &self,
        task: u64,
        mode: Mode,
        phase: Phase,
        kind: Kind,
        step: Option<usize>,
        action: Option<&RequestMetadata>,
    ) -> io::Result<()> {
        let mut file = self.open_for_append()?;
        let record = Record {
            offset: file.metadata()?.len(),
            timestamp: now(),
            instance: &self.instance,
            task,
            phase,
            mode,
            kind,
            step,
            action,
        };
        append_json(&mut file, &record)?;
        file.sync_data()
    }

    fn open_for_append(&self) -> io::Result<File> {
        if !fs::symlink_metadata(&self.path)?.is_file() {
            return Err(io::Error::other("X audit ledger must be a regular file"));
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(&self.path)?;
        file.try_lock().map_err(io::Error::from)?;
        let mut header = [0; HEADER.len()];
        file.read_exact(&mut header)?;
        if header != HEADER {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Unknown X audit ledger schema",
            ));
        }
        recover_tail(&mut file)?;
        file.seek(SeekFrom::End(0))?;
        Ok(file)
    }
}

fn initialize(directory: &Path, path: &Path) -> io::Result<()> {
    let mut staged = tempfile::NamedTempFile::new_in(directory)?;
    staged.write_all(HEADER)?;
    let legacy = directory.join(LEGACY);
    match File::open(legacy) {
        Ok(file) => {
            for line in BufReader::new(file).split(b'\n') {
                let line = line?;
                let Ok(record) = serde_json::from_slice::<LegacyEvent>(&line) else {
                    continue;
                };
                let Some((word, task)) = record.event.split_once(" task ") else {
                    continue;
                };
                let Ok(task) = task.parse::<u64>() else {
                    continue;
                };
                let kind = match word {
                    "approved" => Kind::Approved,
                    "automatic" => Kind::Automatic,
                    "discarded" => Kind::Discarded,
                    _ => continue,
                };
                append_json(
                    &mut staged,
                    &serde_json::json!({
                        "legacy": true,
                        "timestamp": record.timestamp,
                        "task": task,
                        "kind": kind,
                    }),
                )?;
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    staged.as_file().sync_all()?;
    match staged.persist_noclobber(path) {
        Ok(_) => {
            #[cfg(unix)]
            File::open(directory)?.sync_all()?;
            Ok(())
        }
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.error),
    }
}

fn append_json(mut output: impl Write, value: &impl Serialize) -> io::Result<()> {
    let mut row = serde_json::to_vec(value)?;
    if row.len() >= MAX_RECORD {
        return Err(io::Error::other("X audit record exceeds size limit"));
    }
    row.push(b'\n');
    output.write_all(&row)
}

fn recover_tail(file: &mut File) -> io::Result<()> {
    let end = file.metadata()?.len();
    let span = end.min(MAX_RECORD as u64);
    file.seek(SeekFrom::Start(end - span))?;
    let mut tail = [0; MAX_RECORD];
    let tail = &mut tail[..span as usize];
    file.read_exact(tail)?;
    let last = tail
        .iter()
        .rposition(|byte| *byte == b'\n')
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid X audit ledger tail"))?;
    if last + 1 != tail.len() {
        file.set_len(end - span + last as u64 + 1)?;
        file.sync_data()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(path: &Path) -> Vec<serde_json::Value> {
        fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn opens_across_restarts_and_imports_only_typed_legacy_approvals_once() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(FILE);
        let legacy = directory.path().join(LEGACY);
        let old = concat!(
            "{\"timestamp\":100,\"event\":\"approved task 4\"}\n",
            "{\"timestamp\":110,\"event\":\"discarded task 5\"}\n",
            "{\"timestamp\":120,\"event\":\"approved task password=secret\"}\n"
        );
        fs::write(&legacy, old).unwrap();
        for kind in [Kind::Planned, Kind::Finished] {
            Ledger::open(directory.path())
                .unwrap()
                .record(1, Mode::Work, Phase::Completed, kind, None, None)
                .unwrap();
        }
        let log = rows(&path);
        assert_eq!(log.len(), 5);
        assert_eq!(log[0]["schema"], 1);
        assert_eq!(log[1]["kind"], "approved");
        assert_eq!(log[1]["task"], 4);
        assert_eq!(log[2]["kind"], "discarded");
        assert_eq!(log[2]["task"], 5);
        assert_ne!(log[3]["instance"], log[4]["instance"]);
        assert!(log[3]["offset"].as_u64() < log[4]["offset"].as_u64());
        assert_eq!(fs::read_to_string(&legacy).unwrap(), old);
        assert!(!fs::read_to_string(&path).unwrap().contains("secret"));
    }

    #[test]
    fn recovers_partial_appends_without_changing_a_committed_record() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(FILE);
        let ledger = Ledger::open(directory.path()).unwrap();
        ledger
            .record(
                1,
                Mode::Work,
                Phase::AwaitingApproval,
                Kind::Approved,
                None,
                None,
            )
            .unwrap();
        let committed = fs::read(&path).unwrap();
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"offset\": 123, \"torn")
            .unwrap();

        let restarted = Ledger::open(directory.path()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), committed);
        restarted
            .record(1, Mode::Work, Phase::Cancelled, Kind::Finished, None, None)
            .unwrap();
        let complete = fs::read(&path).unwrap();
        assert!(complete.starts_with(&committed));
        assert_eq!(rows(&path).len(), 3);
    }

    #[test]
    fn unknown_schema_is_left_available_to_the_version_that_created_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(FILE);
        let exact = b"{\"schema\":2}\n{\"future\":true}\n";
        fs::write(&path, exact).unwrap();
        assert_eq!(
            Ledger::open(directory.path()).err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(fs::read(&path).unwrap(), exact);
    }
}
