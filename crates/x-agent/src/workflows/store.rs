//! Workflow files on disk: one JSON file per workflow in a private folder. Files are
//! written atomically with owner-only permissions, read with a size cap, and never followed
//! through symlinks. Nothing secret belongs here and nothing here is ever executed as text.

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use super::engine::Report;
use super::model::{Problem, Workflow, normalize};

const MAX_FILE: u64 = 64 * 1024;
const MAX_WORKFLOWS: usize = 100;

/// A workflow as found on disk: valid, or with the reasons it is not runnable.
#[derive(Clone, Debug)]
pub struct Entry {
    pub workflow: Workflow,
    pub problems: Vec<Problem>,
    /// Other workflows that claim one of its trigger phrases.
    pub conflicts: Vec<String>,
}

impl Entry {
    pub fn runnable(&self) -> bool {
        self.problems.is_empty()
    }
}

#[derive(Debug)]
pub enum Match {
    None,
    One(Box<Workflow>),
    /// The phrase belongs to several workflows: nothing runs until one is fixed.
    Conflict {
        phrase: String,
        names: Vec<String>,
    },
    /// The phrase names a workflow that cannot run as it is.
    Broken {
        name: String,
        problems: Vec<Problem>,
    },
}

pub struct Store {
    dir: PathBuf,
    home: PathBuf,
}

impl Store {
    pub fn new(dir: PathBuf, home: PathBuf) -> Self {
        Self { dir, home }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn file(&self, id: &str) -> Result<PathBuf, String> {
        let ok = !id.is_empty()
            && id.len() <= 40
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
        ok.then(|| self.dir.join(format!("{id}.json")))
            .ok_or_else(|| "Not a valid workflow id".into())
    }

    fn read(path: &Path) -> Result<String, String> {
        let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if !meta.is_file() {
            return Err("not a regular file".into());
        }
        if meta.len() > MAX_FILE {
            return Err("file is too large".into());
        }
        std::fs::read_to_string(path).map_err(|e| e.to_string())
    }

    fn write_private(&self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)
            .map_err(|e| format!("could not create {}: {e}", self.dir.display()))?;
        let tmp = path.with_extension("tmp");
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| e.to_string())?;
        f.write_all(bytes)
            .and_then(|()| f.sync_all())
            .map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }

    /// Every workflow file, with its validation result and trigger conflicts.
    pub fn list(&self) -> Vec<Entry> {
        let Ok(read) = std::fs::read_dir(&self.dir) else {
            return vec![];
        };
        let mut found: Vec<Workflow> = read
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .filter(|p| {
                !p.file_name()
                    .is_some_and(|n| n.to_string_lossy().ends_with(".last.json"))
            })
            .take(MAX_WORKFLOWS)
            .filter_map(|p| serde_json::from_str::<Workflow>(&Self::read(&p).ok()?).ok())
            .collect();
        found.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(a.id.cmp(&b.id))
        });
        let triggers: Vec<Vec<String>> = found
            .iter()
            .map(|w| w.triggers.iter().map(|t| normalize(t)).collect())
            .collect();
        found
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let conflicts = found
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != i)
                    .filter(|(j, _)| triggers[i].iter().any(|t| triggers[*j].contains(t)))
                    .map(|(_, o)| o.name.clone())
                    .collect();
                Entry {
                    problems: w.validate(&self.home).err().unwrap_or_default(),
                    workflow: w.clone(),
                    conflicts,
                }
            })
            .collect()
    }

    pub fn get(&self, id: &str) -> Result<Workflow, String> {
        let text = Self::read(&self.file(id)?).map_err(|e| format!("No workflow {id:?} ({e})"))?;
        serde_json::from_str(&text).map_err(|e| format!("Workflow {id:?} is not valid: {e}"))
    }

    /// Validates, checks the triggers against the other workflows, then writes.
    pub fn save(&self, wf: &Workflow) -> Result<(), Vec<Problem>> {
        wf.validate(&self.home)?;
        let mut problems = Vec::new();
        let mine: Vec<String> = wf.triggers.iter().map(|t| normalize(t)).collect();
        for other in self.list().into_iter().filter(|e| e.workflow.id != wf.id) {
            for t in other.workflow.triggers.iter().map(|t| normalize(t)) {
                if mine.contains(&t) {
                    problems.push(Problem {
                        step: None,
                        message: format!(
                            "Trigger {t:?} already belongs to {:?}",
                            other.workflow.name
                        ),
                    });
                }
            }
        }
        let full = self.list().len();
        let exists = self.file(&wf.id).is_ok_and(|p| p.exists());
        if !exists && full >= MAX_WORKFLOWS {
            problems.push(Problem {
                step: None,
                message: format!("At most {MAX_WORKFLOWS} workflows"),
            });
        }
        if !problems.is_empty() {
            return Err(problems);
        }
        let json = serde_json::to_vec_pretty(wf).map_err(|e| {
            vec![Problem {
                step: None,
                message: e.to_string(),
            }]
        })?;
        let path = self.file(&wf.id).map_err(|e| {
            vec![Problem {
                step: None,
                message: e,
            }]
        })?;
        self.write_private(&path, &json).map_err(|e| {
            vec![Problem {
                step: None,
                message: e,
            }]
        })
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        let path = self.file(id)?;
        std::fs::remove_file(&path).map_err(|e| format!("Could not delete {id:?}: {e}"))?;
        let _ = std::fs::remove_file(self.dir.join(format!("{id}.last.json")));
        Ok(())
    }

    /// A copy with a fresh id and name and no triggers (so it cannot collide with the original).
    pub fn duplicate(&self, id: &str) -> Result<Workflow, String> {
        let mut copy = self.get(id)?;
        let taken: Vec<String> = self.list().into_iter().map(|e| e.workflow.id).collect();
        let base = format!("{}-copy", copy.id.chars().take(34).collect::<String>());
        copy.id = (1..)
            .map(|n| {
                if n == 1 {
                    base.clone()
                } else {
                    format!("{base}-{n}")
                }
            })
            .find(|c| !taken.contains(c))
            .unwrap_or(base);
        copy.name = format!("{} (copy)", copy.name.chars().take(50).collect::<String>());
        copy.triggers.clear();
        self.save(&copy).map_err(|p| {
            p.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        })?;
        Ok(copy)
    }

    /// For people who keep workflows in files of their own: the same validation as the editor.
    pub fn import(&self, path: &Path) -> Result<Workflow, String> {
        let text =
            Self::read(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
        let wf: Workflow =
            serde_json::from_str(&text).map_err(|e| format!("Not a valid workflow file: {e}"))?;
        self.save(&wf).map_err(|p| {
            p.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        })?;
        Ok(wf)
    }

    /// Which workflow, if any, this spoken or typed phrase asks for. Matches a trigger
    /// phrase exactly, or "run the <name> workflow". Built-in commands are never consulted
    /// here: callers try those first.
    pub fn find(&self, phrase: &str) -> Match {
        let said = normalize(phrase);
        if said.is_empty() {
            return Match::None;
        }
        let entries = self.list();
        let hits: Vec<&Entry> = entries
            .iter()
            .filter(|e| {
                e.workflow.triggers.iter().any(|t| normalize(t) == said)
                    || by_name(&said, &e.workflow.name)
            })
            .collect();
        match hits.as_slice() {
            [] => Match::None,
            [one] if !one.runnable() => Match::Broken {
                name: one.workflow.name.clone(),
                problems: one.problems.clone(),
            },
            [one]
                if !one.conflicts.is_empty()
                    && one.workflow.triggers.iter().any(|t| normalize(t) == said) =>
            {
                Match::Conflict {
                    phrase: said,
                    names: std::iter::once(one.workflow.name.clone())
                        .chain(one.conflicts.clone())
                        .collect(),
                }
            }
            [one] => Match::One(Box::new(one.workflow.clone())),
            many => Match::Conflict {
                phrase: said,
                names: many.iter().map(|e| e.workflow.name.clone()).collect(),
            },
        }
    }

    pub fn record_last(&self, id: &str, report: &Report) {
        if let (Ok(path), Ok(json)) = (
            self.file(id).map(|p| p.with_extension("last.json")),
            serde_json::to_vec_pretty(report),
        ) {
            let _ = self.write_private(&path, &json);
        }
    }

    pub fn last(&self, id: &str) -> Option<Report> {
        let path = self.file(id).ok()?.with_extension("last.json");
        serde_json::from_str(&Self::read(&path).ok()?).ok()
    }
}

/// "run the coding workflow", "start my coding setup" style phrases for a workflow's name.
fn by_name(said: &str, name: &str) -> bool {
    let name = normalize(name);
    if name.is_empty() {
        return false;
    }
    ["run", "start", "launch"].iter().any(|verb| {
        [
            format!("{verb} {name}"),
            format!("{verb} the {name}"),
            format!("{verb} my {name}"),
            format!("{verb} {name} workflow"),
            format!("{verb} the {name} workflow"),
            format!("{verb} my {name} workflow"),
        ]
        .iter()
        .any(|c| c == said)
    })
}
