use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::{Command, Stdio};

fn git(workspace: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// path -> porcelain status. `None` when the workspace is not a git repo.
pub fn change_snapshot(workspace: &Path) -> Option<BTreeMap<String, String>> {
    let raw = git(
        workspace,
        &["status", "--porcelain", "-z", "--untracked-files=all"],
    )?;
    let mut entries = raw.split('\0');
    let mut snap = BTreeMap::new();
    while let Some(e) = entries.next() {
        if e.len() > 3 {
            snap.insert(e[3..].to_string(), e[..2].to_string());
            if e.starts_with(['R', 'C']) {
                entries.next(); // rename/copy carries the original path as a second field
            }
        }
    }
    Some(snap)
}

pub fn diff_snapshots(
    before: Option<&BTreeMap<String, String>>,
    after: Option<&BTreeMap<String, String>>,
) -> Option<Vec<String>> {
    let (b, a) = (before?, after?);
    let paths: BTreeSet<&String> = b.keys().chain(a.keys()).collect();
    Some(
        paths
            .into_iter()
            .filter(|p| b.get(*p) != a.get(*p))
            .cloned()
            .collect(),
    )
}
