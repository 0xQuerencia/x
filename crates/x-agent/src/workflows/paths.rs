//! Directory arguments for workflows: absolute, inside the home folder, no tricks. A
//! workflow file is configuration the person (or a script) wrote, so every path in it is
//! checked as if it came from anywhere.

use std::path::{Component, Path, PathBuf};

/// Folders that hold credentials. A workflow may not open a terminal or editor in them.
const PRIVATE: [&str; 6] = [
    ".ssh",
    ".gnupg",
    ".aws",
    ".kube",
    ".password-store",
    ".git-credentials",
];
const MAX_PATH: usize = 512;

/// Expands `~`, then requires an absolute path under `home` with no `..` and no private
/// folder. Existence is not checked here (the run's preparation step does that).
pub fn check(raw: &str, home: &Path) -> Result<PathBuf, String> {
    if raw.is_empty() || raw.chars().count() > MAX_PATH || raw.chars().any(char::is_control) {
        return Err("A folder must be 1-512 characters with no control characters".into());
    }
    let expanded = match raw.strip_prefix('~') {
        Some("") => home.to_path_buf(),
        Some(rest) if rest.starts_with('/') => home.join(rest.trim_start_matches('/')),
        Some(_) => return Err("Only ~ for your own home folder is supported".into()),
        None => PathBuf::from(raw),
    };
    if !expanded.is_absolute() {
        return Err("A folder must be an absolute path or start with ~/".into());
    }
    let mut clean = PathBuf::new();
    for part in expanded.components() {
        match part {
            Component::ParentDir => return Err("A folder cannot contain ..".into()),
            Component::CurDir => {}
            other => clean.push(other),
        }
    }
    if !clean.starts_with(home) {
        return Err("A folder must be inside your home folder".into());
    }
    if clean.strip_prefix(home).is_ok_and(|rel| {
        rel.components()
            .any(|c| PRIVATE.iter().any(|p| c.as_os_str() == *p))
    }) {
        return Err("That folder holds credentials and cannot be used".into());
    }
    Ok(clean)
}

/// Like [`check`], and the folder must exist now, with symlinks resolved so a link cannot
/// lead out of the home folder.
pub fn existing(raw: &str, home: &Path) -> Result<PathBuf, String> {
    let wanted = check(raw, home)?;
    let real = wanted
        .canonicalize()
        .map_err(|_| format!("{} does not exist", wanted.display()))?;
    let home_real = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    if !real.is_dir() {
        return Err(format!("{} is not a folder", wanted.display()));
    }
    if !real.starts_with(&home_real) {
        return Err("That folder resolves outside your home folder".into());
    }
    Ok(real)
}

pub fn home() -> Result<PathBuf, String> {
    dirs::home_dir().ok_or_else(|| "No home folder".into())
}
