//! Linux group membership from process status, failing closed on unreadable state.

use super::closure::State;
use std::{fs, io};

// The comm field is parenthesized and may contain spaces or closing parens.
// Only fields after its last closing paren have the fixed numeric format.
pub(super) fn stat_fields(bytes: &[u8]) -> Option<(i32, bool)> {
    let end = bytes.iter().rposition(|byte| *byte == b')')?;
    let tail = std::str::from_utf8(bytes.get(end + 1..)?).ok()?;
    let fields: Vec<_> = tail.split_whitespace().collect();
    let group = fields.get(2)?.parse().ok()?;
    let threads: u32 = fields.get(17)?.parse().ok()?;
    let dead = matches!(*fields.first()?, "Z" | "X") && threads <= 1;
    Some((group, dead))
}

pub(super) fn group_state(pgid: i32) -> State {
    let Ok(entries) = fs::read_dir("/proc") else {
        return State::Unavailable;
    };
    let mut saw_leader = false;
    for entry in entries {
        let Ok(entry) = entry else {
            return State::Unavailable;
        };
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|name| name.parse::<i32>().ok()) else {
            continue;
        };
        let bytes = match fs::read(entry.path().join("stat")) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => return State::Unavailable,
        };
        let Some((group, dead)) = stat_fields(&bytes) else {
            return State::Unavailable;
        };
        if pid == pgid {
            saw_leader = group == pgid;
        }
        if group == pgid && !dead {
            return State::Active;
        }
    }
    if saw_leader {
        State::Empty
    } else {
        State::Unavailable
    }
}
