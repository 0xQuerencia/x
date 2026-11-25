//! Suspend before assignment: the provider cannot create children outside our Job
//! during the spawn/assign interval. The Job is ownership, not a security sandbox.

use std::io;
use std::mem::{offset_of, size_of};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::{Child, Command, ExitStatus};
use std::ptr::{null, null_mut};
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_BASIC_LIMIT_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED, GetProcessIdOfThread, OpenThread, ResumeThread,
    THREAD_QUERY_LIMITED_INFORMATION, THREAD_SUSPEND_RESUME,
};

use super::closure::{self, State};
use super::{Event, EventKind, EventSink, Finished, KILL_GRACE, TerminationEvidence};
use crate::cancellation::Cancellation;

struct Job(OwnedHandle);

impl Job {
    fn new() -> Result<Self, String> {
        // SAFETY: null attributes make this unnamed handle non-inheritable.
        let raw = unsafe { CreateJobObjectW(null(), null()) };
        let job = Self(owned_handle(raw, "CreateJobObjectW")?);
        let limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
            BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION {
                // No BREAKAWAY_OK or SILENT_BREAKAWAY_OK: ordinary descendants
                // inherit this Job, including providers that create child jobs.
                LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                ..Default::default()
            },
            ..Default::default()
        };
        // SAFETY: the handle and the correctly sized native struct are live.
        let ok = unsafe {
            SetInformationJobObject(
                job.raw(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        checked(ok, "SetInformationJobObject")?;
        Ok(job)
    }

    fn raw(&self) -> HANDLE {
        self.0.as_raw_handle()
    }

    fn assign(&self, child: &Child) -> Result<(), String> {
        // SAFETY: both handles are retained; the child's primary thread is suspended.
        let ok = unsafe { AssignProcessToJobObject(self.raw(), child.as_raw_handle()) };
        checked(ok, "AssignProcessToJobObject")
    }

    fn active(&self) -> Result<u32, String> {
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: the output buffer has the documented layout and size.
        let ok = unsafe {
            QueryInformationJobObject(
                self.raw(),
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                null_mut(),
            )
        };
        checked(ok, "QueryInformationJobObject")?;
        Ok(accounting.ActiveProcesses)
    }

    fn terminate(&self) {
        // SAFETY: this anonymous Job contains only this run's assigned process
        // and inherited descendants. Success alone is not closure evidence.
        unsafe { TerminateJobObject(self.raw(), 1) };
    }
}

pub(super) struct ChildGuard {
    child: Option<Child>,
    exit: Option<ExitStatus>,
    job: Job,
    events: EventSink,
    closed: bool,
}

impl ChildGuard {
    pub(super) fn spawn(
        cmd: &mut Command,
        stop: &Cancellation,
        events: EventSink,
    ) -> Result<Self, String> {
        let mut owned = Self::spawn_suspended(cmd, stop, events)?;
        owned.resume(stop)?;
        Ok(owned)
    }

    fn spawn_suspended(
        cmd: &mut Command,
        stop: &Cancellation,
        events: EventSink,
    ) -> Result<Self, String> {
        stop.check()?;
        let job = Job::new()?;
        cmd.creation_flags(CREATE_SUSPENDED | CREATE_NEW_PROCESS_GROUP);
        let child = crate::retry_busy(|| {
            if stop.requested() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "Stopped"));
            }
            cmd.spawn()
        })
        .map_err(|error| error.to_string())?;
        let owned = Self {
            child: Some(child),
            exit: None,
            job,
            events,
            closed: false,
        };
        // On failure Drop kills the retained direct child, even if assignment
        // never succeeded. No provider code has run before this point.
        owned.job.assign(owned.child.as_ref().unwrap())?;
        Ok(owned)
    }

    fn resume(&mut self, stop: &Cancellation) -> Result<(), String> {
        let child = self.child.as_mut().unwrap();
        let thread = primary_thread(child.id(), stop)?;
        if child
            .try_wait()
            .map_err(|error| error.to_string())?
            .is_some()
        {
            return Err("Owned process exited before startup".into());
        }
        // The thread handle pins its identity; never resume a recycled thread ID.
        // SAFETY: this query takes the live OpenThread handle, not an arbitrary ID.
        if unsafe { GetProcessIdOfThread(thread.as_raw_handle()) } != child.id() {
            return Err("Owned primary thread identity changed".into());
        }
        stop.check()?;
        // SAFETY: assignment succeeded, and this is the one observed thread of
        // our still-suspended process. Only a previous count of 1 is accepted.
        let previous = unsafe { ResumeThread(thread.as_raw_handle()) };
        if previous != 1 {
            return Err("Owned primary thread did not resume from its initial suspension".into());
        }
        stop.check()
    }

    pub(super) fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().unwrap()
    }

    /// Release the direct process handle only after exit. Job accounting can
    /// retain exited members while process handles remain open.
    fn observe_exit(&mut self) -> Result<bool, io::Error> {
        let Some(child) = self.child.as_mut() else {
            return Ok(self.exit.is_some());
        };
        if let Some(status) = child.try_wait()? {
            self.exit = Some(status);
            self.child.take();
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn request_termination(&mut self) {
        self.job.terminate();
        if let Some(child) = self.child.as_mut() {
            // The retained handle also covers a failed Job assignment. No PID
            // lookup or external taskkill process can target an unrelated child.
            let _ = child.kill();
        }
    }

    fn close(&mut self, mut forced: bool, stop: &Cancellation) -> TerminationEvidence {
        let start = Instant::now();
        let mut last_attempt = None::<Instant>;
        let events = self.events.clone();
        let elapsed = closure::wait_until_empty(
            KILL_GRACE,
            || {
                forced |= stop.requested() || start.elapsed() >= KILL_GRACE;
                if forced
                    && last_attempt.is_none_or(|last| last.elapsed() >= Duration::from_secs(1))
                {
                    self.request_termination();
                    last_attempt = Some(Instant::now());
                }
                match self.observe_exit() {
                    Ok(true) => match self.job.active() {
                        Ok(0) => State::Empty,
                        Ok(_) => State::Active,
                        Err(_) => State::Unavailable,
                    },
                    Ok(false) => State::Active,
                    Err(_) => State::Unavailable,
                }
            },
            || {
                // This bounded static callback cannot leak provider output. A
                // callback panic must not release live process ownership.
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    events(Event::new(
                        EventKind::TerminationUnconfirmed,
                        "Windows has not confirmed the owned Job is empty. X is still stopping it and retains the project lease; new writes remain blocked.",
                    ));
                }));
            },
        );
        self.closed = true;
        TerminationEvidence::WindowsJobEmpty {
            elapsed_ms: elapsed.as_millis().try_into().unwrap_or(u64::MAX),
            forced,
        }
    }

    pub(super) fn finish(&mut self, stop: &Cancellation, failed: bool) -> Finished {
        let deadline = Instant::now() + KILL_GRACE;
        if !failed {
            while !stop.requested() && Instant::now() < deadline {
                if matches!(self.observe_exit(), Ok(true)) {
                    break;
                }
                thread::sleep(Duration::from_millis(25));
            }
        }
        let forced = failed || stop.requested() || self.child.is_some();
        let termination = self.close(forced, stop);
        let TerminationEvidence::WindowsJobEmpty { forced, .. } = termination;
        Finished {
            exit_code: self.exit.and_then(|status| status.code()),
            termination: Some(termination),
            failure: (forced && !failed && !stop.requested())
                .then_some("left owned processes running after closing its output"),
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !self.closed {
            // Also covers supervisor creation failure and unwinding. Keep the
            // outer execution/project lease alive until closure is proved.
            self.close(true, &Cancellation::default());
        }
    }
}

fn primary_thread(pid: u32, stop: &Cancellation) -> Result<OwnedHandle, String> {
    // SAFETY: the snapshot is read-only and owned below.
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    let snapshot = owned_handle(raw, "CreateToolhelp32Snapshot")?;
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut thread_id = None;
    let deadline = Instant::now() + Duration::from_secs(2);
    // SAFETY: the initialized entry is the correct size for this snapshot API.
    let mut found = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
    let mut count = 0;
    while found != 0 {
        stop.check()?;
        count += 1;
        if count > 65_536 || Instant::now() >= deadline {
            return Err("Owned thread discovery exceeded its bound".into());
        }
        if entry.dwSize < (offset_of!(THREADENTRY32, th32OwnerProcessID) + size_of::<u32>()) as u32
        {
            return Err("Windows returned an incomplete thread identity".into());
        }
        if entry.th32OwnerProcessID == pid && thread_id.replace(entry.th32ThreadID).is_some() {
            return Err("Owned suspended process has ambiguous primary threads".into());
        }
        entry.dwSize = size_of::<THREADENTRY32>() as u32;
        // SAFETY: the snapshot and initialized output entry remain live.
        found = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
        return Err(format!("Could not enumerate owned threads: {error}"));
    }
    let id = thread_id.ok_or("Owned primary thread was not found")?;
    // SAFETY: only a candidate handle is opened here; resume revalidates its PID
    // while retaining the original process handle before any mutation.
    let raw = unsafe {
        OpenThread(
            THREAD_SUSPEND_RESUME | THREAD_QUERY_LIMITED_INFORMATION,
            0,
            id,
        )
    };
    owned_handle(raw, "OpenThread")
}

fn checked(ok: i32, operation: &str) -> Result<(), String> {
    if ok == 0 {
        Err(format!("{operation}: {}", io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

fn owned_handle(raw: HANDLE, operation: &str) -> Result<OwnedHandle, String> {
    if raw.is_null() || raw == INVALID_HANDLE_VALUE {
        return Err(format!("{operation}: {}", io::Error::last_os_error()));
    }
    // SAFETY: the successful Win32 call transferred a new handle to this owner.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

#[cfg(test)]
mod tests;
