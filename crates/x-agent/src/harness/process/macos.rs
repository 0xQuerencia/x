//! macOS group membership from libproc. The unreaped root anchors the scan.

use std::mem::{size_of, zeroed};

use super::closure::State;

// Darwin's <sys/proc_info.h>.
const PROC_PGRP_ONLY: u32 = 2;
const MAX_GROUP_PIDS: usize = 1024;

pub(super) fn group_state(pgid: i32) -> State {
    let mut members = [0_i32; MAX_GROUP_PIDS];
    // SAFETY: the fixed buffer is writable for exactly the supplied size.
    let bytes = unsafe {
        libc::proc_listpids(
            PROC_PGRP_ONLY,
            pgid as u32,
            members.as_mut_ptr().cast(),
            size_of::<[i32; MAX_GROUP_PIDS]>() as i32,
        )
    };
    let pid_size = size_of::<i32>() as i32;
    if bytes <= 0 || bytes % pid_size != 0 || bytes / pid_size >= MAX_GROUP_PIDS as i32 {
        return State::Unavailable;
    }
    let mut saw_root = false;
    for &pid in &members[..(bytes / pid_size) as usize] {
        // SAFETY: an all-zero placeholder is valid for this integer struct.
        let mut info: libc::proc_bsdshortinfo = unsafe { zeroed() };
        let expected = size_of::<libc::proc_bsdshortinfo>() as i32;
        // SAFETY: info is writable for expected bytes. Arg 1 includes zombies.
        let read = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDT_SHORTBSDINFO,
                1,
                (&mut info as *mut libc::proc_bsdshortinfo).cast(),
                expected,
            )
        };
        if read != expected || info.pbsi_pid != pid as u32 || info.pbsi_pgid != pgid as u32 {
            return State::Unavailable;
        }
        saw_root |= pid == pgid;
        if info.pbsi_status != libc::SZOMB {
            return State::Active;
        }
    }
    if saw_root {
        State::Empty
    } else {
        State::Unavailable
    }
}
