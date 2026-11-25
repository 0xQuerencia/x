use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

fn git(workspace: &Path, args: &[&str]) -> Option<String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(workspace)
        .args(["-c", "core.fsmonitor=false"])
        .args(args);
    // Evidence belongs to the approved checkout, not an inherited Git override.
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    ] {
        command.env_remove(key);
    }
    let (ok, bytes) = crate::desktop::capture(&mut command, Duration::from_secs(10)).ok()?;
    if !ok {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// path -> porcelain status. `None` when the workspace is not a git repo.
pub fn change_snapshot(workspace: &Path) -> Option<BTreeMap<String, String>> {
    let raw = git(
        workspace,
        &[
            "status",
            "--porcelain",
            "-z",
            "--untracked-files=all",
            "--no-renames",
        ],
    )?;
    parse_status(&raw)
}

fn parse_status(raw: &str) -> Option<BTreeMap<String, String>> {
    if !raw.is_empty() && !raw.ends_with('\0') {
        return None;
    }
    let mut snap = BTreeMap::new();
    for entry in raw.split_terminator('\0') {
        let bytes = entry.as_bytes();
        if bytes.len() < 4
            || bytes[2] != b' '
            || !bytes[..2].iter().all(|&b| b" MADT?!U".contains(&b))
        {
            return None;
        }
        if snap
            .insert(entry[3..].to_string(), entry[..2].to_string())
            .is_some()
        {
            return None;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_evidence_rejects_incomplete_and_unexpected_records() {
        for invalid in [" M file", "R  new\0old\0", " M file\0 M file\0", "é file\0"] {
            assert!(parse_status(invalid).is_none(), "{invalid:?}");
        }
    }

    #[test]
    fn paths_with_spaces_and_utf8_are_preserved() {
        let result = parse_status(" M a b\0?? café\0").unwrap();
        assert_eq!(result.get("a b").map(String::as_str), Some(" M"));
        assert_eq!(result.get("café").map(String::as_str), Some("??"));
        assert!(parse_status("").unwrap().is_empty());
    }
}
