use super::*;
use std::os::unix::fs::PermissionsExt;

fn fake_hex(dir: &std::path::Path, script: &str) -> HexCli {
    let p = dir.join("hex");
    std::fs::write(&p, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    HexCli {
        exe: p.to_string_lossy().into_owned(),
    }
}

#[test]
fn stop_print_returns_the_transcript() {
    let d = tempfile::tempdir().unwrap();
    let h = fake_hex(d.path(), "echo '  open firefox '");
    assert_eq!(h.stop_print().unwrap(), "open firefox");
}

#[test]
fn exit_four_means_no_speech() {
    let d = tempfile::tempdir().unwrap();
    assert_eq!(
        fake_hex(d.path(), "exit 4").stop_print(),
        Err(SttError::NoSpeech)
    );
}

#[test]
fn hex_not_running_is_actionable() {
    let d = tempfile::tempdir().unwrap();
    let e = fake_hex(d.path(), "exit 3")
        .start()
        .unwrap_err()
        .to_string();
    assert!(e.contains("hex start"), "{e}");
}

#[test]
fn cancel_falls_back_when_the_installed_hex_lacks_it() {
    let d = tempfile::tempdir().unwrap();
    let log = d.path().join("log");
    let h = fake_hex(
        d.path(),
        &format!(
            "echo \"$@\" >> {}\nif [ \"$2\" = cancel ]; then exit 2; fi\nexit 4",
            log.display()
        ),
    );
    h.cancel().unwrap();
    let calls = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        calls.lines().collect::<Vec<_>>(),
        ["dictation cancel", "dictation stop --print"]
    );
}

#[test]
fn missing_binary_is_reported() {
    let h = HexCli {
        exe: "/nonexistent/hex".into(),
    };
    assert!(h.start().unwrap_err().to_string().contains("not found"));
}

#[test]
fn a_hung_hex_times_out() {
    let d = tempfile::tempdir().unwrap();
    let h = fake_hex(d.path(), "sleep 30");
    let e = h.run(&["x"], Duration::from_millis(300)).err().unwrap();
    assert!(e.to_string().contains("timed out"));
}
