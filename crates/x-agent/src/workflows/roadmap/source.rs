use std::fs::{File, Metadata};
use std::io::Read;
use std::path::{Path, PathBuf};

use super::{Document, MAX_BYTES, digest, parse};

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub(crate) struct Snapshot {
    pub path: PathBuf,
    pub sha256: String,
    identity: Identity,
}

impl Snapshot {
    pub(crate) fn identity_hash(&self) -> Result<String, String> {
        serde_json::to_vec(&self.identity)
            .map(|bytes| digest(&bytes))
            .map_err(|_| "Cannot bind the roadmap file identity".into())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
struct Identity {
    length: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

fn identity(meta: &Metadata) -> Identity {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Identity {
        length: meta.len(),
        modified: meta.modified().ok(),
        #[cfg(unix)]
        device: meta.dev(),
        #[cfg(unix)]
        inode: meta.ino(),
    }
}

/// Walk every relative component using no-follow handles on Unix.
#[cfg(unix)]
fn open(home: &Path, relative: &Path) -> Result<File, String> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Component;
    let mut parent = File::open(home).map_err(|_| "Cannot open the roadmap home directory")?;
    let parts = relative.components().collect::<Vec<_>>();
    for (index, part) in parts.iter().enumerate() {
        let Component::Normal(name) = part else {
            return Err("Invalid roadmap path component".into());
        };
        let name = CString::new(name.as_bytes()).map_err(|_| "Invalid roadmap path")?;
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | if index + 1 == parts.len() {
                libc::O_NONBLOCK
            } else {
                libc::O_DIRECTORY
            };
        // SAFETY: the directory handle and NUL-terminated component remain alive.
        let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err("Cannot read the selected roadmap without following links".into());
        }
        // SAFETY: openat returned a new owned descriptor, transferred exactly once.
        parent = unsafe { File::from_raw_fd(fd) };
    }
    Ok(parent)
}

#[cfg(not(unix))]
fn open(_: &Path, _: &Path) -> Result<File, String> {
    Err("Secure roadmap file reading is unavailable on this platform; a native no-follow reader is required".into())
}

pub(crate) fn read(path: &Path, home: &Path) -> Result<(Snapshot, Document), String> {
    let raw = path.to_str().ok_or("The roadmap path must be UTF-8")?;
    let canonical_home = home
        .canonicalize()
        .map_err(|_| "Cannot resolve the roadmap home directory")?;
    let wanted = super::super::paths::check(raw, home)
        .or_else(|_| super::super::paths::check(raw, &canonical_home))?;
    let relative = wanted
        .strip_prefix(home)
        .or_else(|_| wanted.strip_prefix(&canonical_home))
        .map_err(|_| "The roadmap must be inside your home directory")?;
    if !relative.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
    }) {
        return Err("Choose an explicit .md or .markdown roadmap file".into());
    }
    let mut file = open(&canonical_home, relative)?;
    let before = file
        .metadata()
        .map_err(|_| "Cannot inspect the opened roadmap")?;
    if !before.is_file() || before.len() > MAX_BYTES as u64 {
        return Err("Roadmap must be a regular file of at most 64 KiB".into());
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read the selected roadmap")?;
    let after = file
        .metadata()
        .map_err(|_| "Cannot recheck the opened roadmap")?;
    if bytes.len() > MAX_BYTES || identity(&before) != identity(&after) {
        return Err("The roadmap changed during reading or exceeded 64 KiB".into());
    }
    let current = open(&canonical_home, relative)?
        .metadata()
        .map_err(|_| "Cannot recheck the roadmap identity")?;
    if identity(&after) != identity(&current) {
        return Err("The selected roadmap changed identity during reading".into());
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| "Roadmap must be UTF-8")?;
    let document = parse(text)?;
    Ok((
        Snapshot {
            path: canonical_home.join(relative),
            sha256: digest(&bytes),
            identity: identity(&after),
        },
        document,
    ))
}
