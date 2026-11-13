//! On-demand process viewer with an explicit, verified termination action.
//! Linux uses /proc and pidfds, never shell commands or PID-only termination.
//! Unsupported systems return no entries and refuse termination.

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub resident_kib: u64,
    /// Linux kernel starttime from /proc/<pid>/stat; prevents acting on a reused PID.
    pub start_ticks: u64,
}

#[cfg(target_os = "linux")]
mod linux {
    use super::ProcessInfo;
    use std::fs;
    use std::os::fd::FromRawFd;
    use std::time::{Duration, Instant};

    const MAX_SCAN: usize = 4_000;
    const SCAN_BUDGET: Duration = Duration::from_millis(65);

    fn status(status: &str) -> Option<(String, u32, u64)> {
        let mut name = None;
        let mut uid = None;
        let mut rss = 0;
        for line in status.lines() {
            if let Some(value) = line.strip_prefix("Name:") {
                name = Some(value.trim().chars().take(80).collect::<String>());
            } else if let Some(value) = line.strip_prefix("Uid:") {
                uid = value.split_whitespace().next()?.parse::<u32>().ok();
            } else if let Some(value) = line.strip_prefix("VmRSS:") {
                rss = value.split_whitespace().next()?.parse::<u64>().ok()?;
            }
        }
        Some((name?, uid?, rss))
    }

    fn start_ticks(stat: &str) -> Option<u64> {
        // The kernel wraps comm in parentheses; it can contain spaces and ')'.
        // Fields after that are numbered 3 onward, making starttime (22) index 19.
        let at = stat.rfind(')')?;
        stat[at + 1..].split_whitespace().nth(19)?.parse().ok()
    }

    fn read(pid: u32) -> Option<(ProcessInfo, u32)> {
        let base = format!("/proc/{pid}");
        let (name, uid, resident_kib) =
            status(&fs::read_to_string(format!("{base}/status")).ok()?)?;
        let start_ticks = start_ticks(&fs::read_to_string(format!("{base}/stat")).ok()?)?;
        Some((
            ProcessInfo {
                pid,
                name,
                resident_kib,
                start_ticks,
            },
            uid,
        ))
    }

    pub(super) fn list(query: &str) -> Vec<ProcessInfo> {
        let needle = query.trim().to_lowercase();
        let uid = unsafe { libc::getuid() };
        let started = Instant::now();
        let Ok(entries) = fs::read_dir("/proc") else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for (i, entry) in entries.flatten().enumerate() {
            if i > MAX_SCAN || started.elapsed() > SCAN_BUDGET {
                break;
            }
            let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            if pid <= 1 || pid == std::process::id() {
                continue;
            }
            let Some((info, owner)) = read(pid) else {
                continue;
            };
            if owner == uid && info.name.to_lowercase().contains(&needle) {
                found.push(info);
            }
        }
        found.sort_by(|a, b| {
            b.resident_kib
                .cmp(&a.resident_kib)
                .then_with(|| a.pid.cmp(&b.pid))
        });
        found.truncate(6);
        found
    }

    pub(super) fn terminate(selected: &ProcessInfo) -> Result<String, String> {
        if selected.pid <= 1 || selected.pid == std::process::id() {
            return Err("X refuses to terminate the system or its own process".into());
        }
        let uid = unsafe { libc::getuid() };
        if uid == 0 {
            return Err("Process termination from a root-owned X session is disabled".into());
        }
        let pid = i32::try_from(selected.pid).map_err(|_| "Invalid PID")?;
        // pidfd pins the actual process, preventing signalling a different process
        // if its numeric PID is recycled after the user selected a row.
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if raw < 0 {
            return Err(format!(
                "The kernel cannot safely pin this process: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: pidfd_open returned a new owned file descriptor.
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw as i32) };
        let Some((current, owner)) = read(selected.pid) else {
            return Err("The selected process has already exited".into());
        };
        if owner != uid || current.start_ticks != selected.start_ticks || current.name != selected.name
        {
            return Err("The selected process has changed; choose it again".into());
        }
        use std::os::fd::AsRawFd;
        // SAFETY: pidfd is open and verified; null siginfo is supported by this syscall.
        let sent = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                fd.as_raw_fd(),
                libc::SIGTERM,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        if sent != 0 {
            return Err(format!(
                "Could not signal the selected process: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(format!("Sent SIGTERM to {} (PID {}); exit is not verified", current.name, current.pid))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_process_identity_from_stat_with_spaces() {
            // starttime is field 22, after the 19th post-comm field.
            let rest = (0..24).map(|i| i.to_string()).collect::<Vec<_>>().join(" ");
            let parsed = format!("432 (name with ) brackets) {rest}");
            assert_eq!(start_ticks(&parsed), Some(19));
        }

        #[test]
        fn reads_status_uid_and_memory() {
            let sample = "Name:\tfirefox\nUid:\t1000\t1000\t1000\t1000\nVmRSS:\t2048 kB\n";
            assert_eq!(status(sample), Some(("firefox".into(), 1000, 2048)));
        }

        #[test]
        fn refuses_to_terminate_pid_one() {
            let selected = ProcessInfo {
                pid: 1,
                name: "init".into(),
                resident_kib: 12,
                start_ticks: 10,
            };
            assert!(terminate(&selected).is_err());
        }
    }
}

pub fn list(query: &str) -> Vec<ProcessInfo> {
    #[cfg(target_os = "linux")]
    {
        linux::list(query)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = query;
        Vec::new()
    }
}

pub fn terminate(selected: &ProcessInfo) -> Result<String, String> {
    #[cfg(target_os = "linux")]
    {
        linux::terminate(selected)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = selected;
        Err("Native process control is not implemented on this platform".into())
    }
}
