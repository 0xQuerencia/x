//! Bounded desktop system controls. All commands are fixed argv, never a shell.
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Only explicitly supported actions reach the operating system.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    SetVolume(u8),
    SetMute(bool),
    SetBrightness(u8),
    Play,
    Pause,
    Next,
    Previous,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Capability {
    Audio,
    Brightness,
    Media,
}

pub trait Runner {
    fn exists(&self, program: &str) -> bool;
    fn run(&self, program: &str, args: &[&str]) -> Result<String, String>;
}

pub struct Local;

impl Runner for Local {
    fn exists(&self, program: &str) -> bool {
        crate::tools::which(program).is_some()
    }

    fn run(&self, program: &str, args: &[&str]) -> Result<String, String> {
        if !self.exists(program) {
            return Err(format!(
                "{program} is unavailable; install it or use a different backend"
            ));
        }
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| format!("could not run {program}"))?;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        return Err(format!("{program} refused the requested change"));
                    }
                    let mut stdout = child.stdout.take().ok_or("No command output")?;
                    use std::io::Read;
                    let mut bytes = Vec::new();
                    stdout
                        .by_ref()
                        .take(8193)
                        .read_to_end(&mut bytes)
                        .map_err(|_| "Failed reading system control output")?;
                    if bytes.len() > 8192 {
                        return Err("System control output exceeded 8 KiB".into());
                    }
                    return String::from_utf8(bytes)
                        .map_err(|_| "System control output was not UTF-8".into());
                }
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{program} timed out"));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("could not read {program} status"));
                }
            }
        }
    }
}

/// Determine capabilities without changing state.
pub fn capabilities(runner: &dyn Runner) -> Vec<Capability> {
    #[cfg(target_os = "macos")]
    {
        if runner.exists("osascript") {
            vec![Capability::Audio]
        } else {
            Vec::new()
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let mut found = Vec::new();
        if runner.exists("wpctl") || runner.exists("pactl") {
            found.push(Capability::Audio);
        }
        if runner.exists("brightnessctl") {
            found.push(Capability::Brightness);
        }
        if runner.exists("playerctl") {
            found.push(Capability::Media);
        }
        found
    }
}

#[cfg(any(test, not(target_os = "macos")))]
fn percent_from_wpctl(text: &str) -> Result<(u8, bool), String> {
    let value = text
        .strip_prefix("Volume:")
        .and_then(|t| t.split_whitespace().next())
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite() && (0.0..=1.5).contains(v))
        .ok_or("Audio backend returned an invalid volume")?;
    Ok((
        (value * 100.0).round().min(100.0) as u8,
        text.contains("[MUTED]"),
    ))
}

#[cfg(any(test, not(target_os = "macos")))]
fn percent_from_pactl(text: &str) -> Result<u8, String> {
    text.split_whitespace()
        .find_map(|part| {
            part.trim_matches(|ch: char| !(ch.is_ascii_digit() || ch == '%'))
                .strip_suffix('%')
                .and_then(|n| n.parse::<u8>().ok())
        })
        .ok_or_else(|| "Audio backend returned an invalid volume".into())
}

/// AppleScript's volume settings record, read without using System Events
/// or a privileged helper. Unrecognized data fails closed.
#[cfg(any(test, target_os = "macos"))]
fn percent_from_macos(text: &str) -> Result<(u8, bool), String> {
    let mut level = None;
    let mut muted = None;
    for part in text.split(',').map(str::trim) {
        if let Some(value) = part.strip_prefix("output volume:") {
            level = value
                .trim()
                .parse::<u8>()
                .ok()
                .filter(|value| *value <= 100);
        }
        if let Some(value) = part.strip_prefix("output muted:") {
            muted = match value.trim() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            };
        }
    }
    Ok((
        level.ok_or("macOS returned no valid output volume")?,
        muted.ok_or("macOS returned no valid mute state")?,
    ))
}

fn volume(runner: &dyn Runner) -> Result<(u8, bool), String> {
    #[cfg(target_os = "macos")]
    {
        percent_from_macos(&runner.run("osascript", &["-e", "get volume settings"])?)
    }
    #[cfg(not(target_os = "macos"))]
    {
        if runner.exists("wpctl") {
            return percent_from_wpctl(
                &runner.run("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"])?,
            );
        }
        if runner.exists("pactl") {
            let level = runner.run("pactl", &["get-sink-volume", "@DEFAULT_SINK@"])?;
            let mute = runner.run("pactl", &["get-sink-mute", "@DEFAULT_SINK@"])?;
            return Ok((percent_from_pactl(&level)?, mute.contains("yes")));
        }
        Err("Neither wpctl nor pactl is installed".into())
    }
}

fn set_volume(runner: &dyn Runner, level: u8) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        // Only a checked 0-100 integer enters this fixed AppleScript statement.
        runner.run(
            "osascript",
            &["-e", &format!("set volume output volume {level}")],
        )?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        let pct = format!("{level}%");
        if runner.exists("wpctl") {
            runner.run("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", &pct])?;
        } else {
            runner.run("pactl", &["set-sink-volume", "@DEFAULT_SINK@", &pct])?;
        }
    }
    let (actual, _) = volume(runner)?;
    if actual.abs_diff(level) > 1 {
        return Err(format!(
            "Volume did not reach {level}% (read back {actual}%)"
        ));
    }
    Ok(())
}

fn set_mute(runner: &dyn Runner, muted: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let script = if muted {
            "set volume with output muted"
        } else {
            "set volume without output muted"
        };
        runner.run("osascript", &["-e", script])?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        let value = if muted { "1" } else { "0" };
        if runner.exists("wpctl") {
            runner.run("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", value])?;
        } else {
            runner.run("pactl", &["set-sink-mute", "@DEFAULT_SINK@", value])?;
        }
    }
    if volume(runner)?.1 != muted {
        return Err("Audio mute state did not change as requested".into());
    }
    Ok(())
}

fn brightness(runner: &dyn Runner) -> Result<u8, String> {
    let current: u64 = runner
        .run("brightnessctl", &["get"])?
        .trim()
        .parse()
        .map_err(|_| "Invalid current brightness")?;
    let max: u64 = runner
        .run("brightnessctl", &["max"])?
        .trim()
        .parse()
        .map_err(|_| "Invalid maximum brightness")?;
    if max == 0 || current > max {
        return Err("Invalid brightness range".into());
    }
    Ok(((u128::from(current) * 100 + u128::from(max) / 2) / u128::from(max)).min(100) as u8)
}

fn set_brightness(runner: &dyn Runner, level: u8) -> Result<(), String> {
    runner.run("brightnessctl", &["set", &format!("{level}%")])?;
    let actual = brightness(runner)?;
    if actual.abs_diff(level) > 2 {
        return Err(format!(
            "Brightness did not reach {level}% (read back {actual}%)"
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
enum Undo {
    Volume { before: u8, after: u8 },
    Mute { before: bool, after: bool },
    Brightness { before: u8, after: u8 },
}

impl Undo {
    /// Refuse to overwrite a setting someone changed outside X since this
    /// command. Exact mute state and 1% tolerances match action readback.
    fn verify_current(self, runner: &dyn Runner) -> Result<(), String> {
        let current = match self {
            Self::Volume { after, .. } => volume(runner)?.0.abs_diff(after) <= 1,
            Self::Mute { after, .. } => volume(runner)?.1 == after,
            Self::Brightness { after, .. } => brightness(runner)?.abs_diff(after) <= 2,
        };
        if current {
            Ok(())
        } else {
            Err("Cannot undo: the system setting changed since X last controlled it".into())
        }
    }
}

static HISTORY: Mutex<Vec<(Instant, Undo)>> = Mutex::new(Vec::new());

fn remember(previous: Undo) {
    let mut history = HISTORY.lock().unwrap_or_else(|p| p.into_inner());
    if history.len() == 16 {
        history.remove(0);
    }
    history.push((Instant::now(), previous));
}

/// Most recently verified and undoable system control, without touching IO.
pub fn latest_undo_at() -> Option<Instant> {
    HISTORY
        .lock()
        .ok()
        .and_then(|history| history.last().map(|(at, _)| *at))
}

pub fn recent_undo_available() -> bool {
    latest_undo_at().is_some_and(|at| at.elapsed() <= Duration::from_secs(8))
}

pub fn execute(op: Op, runner: &dyn Runner) -> Result<String, String> {
    match op {
        Op::SetVolume(level) if level <= 100 => {
            let (before, _) = volume(runner)?;
            set_volume(runner, level)?;
            remember(Undo::Volume {
                before,
                after: volume(runner)?.0,
            });
            Ok(format!("Volume set and verified at {level}%"))
        }
        Op::SetMute(muted) => {
            let (_, before) = volume(runner)?;
            set_mute(runner, muted)?;
            remember(Undo::Mute {
                before,
                after: muted,
            });
            Ok(if muted {
                "Audio muted (verified)"
            } else {
                "Audio unmuted (verified)"
            }
            .into())
        }
        Op::SetBrightness(level) if level <= 100 => {
            let before = brightness(runner)?;
            set_brightness(runner, level)?;
            remember(Undo::Brightness {
                before,
                after: brightness(runner)?,
            });
            Ok(format!("Brightness set and verified at {level}%"))
        }
        Op::SetVolume(_) | Op::SetBrightness(_) => Err("Percent must be 0-100".into()),
        Op::Play | Op::Pause | Op::Next | Op::Previous => {
            let command = match op {
                Op::Play => "play",
                Op::Pause => "pause",
                Op::Next => "next",
                Op::Previous => "previous",
                _ => unreachable!(),
            };
            runner.run("playerctl", &[command])?;
            if matches!(op, Op::Play | Op::Pause) {
                let status = runner.run("playerctl", &["status"])?;
                let expected = if op == Op::Play { "Playing" } else { "Paused" };
                if status.trim() != expected {
                    return Err("Media player did not confirm the requested playback state".into());
                }
                return Ok(format!("Media {expected} (verified)"));
            }
            Ok(format!(
                "Media {command} sent; track transition not verified"
            ))
        }
    }
}

/// Reverses only a verified volume, mute or brightness change.
/// As with window Undo, a stale state must not rewrite a current setting.
pub fn undo(runner: &dyn Runner) -> Result<String, String> {
    let previous = HISTORY.lock().unwrap_or_else(|p| p.into_inner()).pop();
    let (at, previous) = previous.ok_or("There is no system-control change to undo")?;
    if at.elapsed() > Duration::from_secs(600) {
        HISTORY.lock().unwrap_or_else(|p| p.into_inner()).clear();
        return Err("System change is too old to undo".into());
    }
    previous.verify_current(runner)?;
    match previous {
        Undo::Volume { before, .. } => {
            set_volume(runner, before)?;
            Ok(format!("Restored volume to {before}%"))
        }
        Undo::Mute { before, .. } => {
            set_mute(runner, before)?;
            Ok("Restored previous mute state".into())
        }
        Undo::Brightness { before, .. } => {
            set_brightness(runner, before)?;
            Ok(format!("Restored brightness to {before}%"))
        }
    }
}

/// Direct, exact command phrases only. No arbitrary audio or shell input.
pub fn parse(text: &str) -> Option<Op> {
    let clean = text.trim().to_ascii_lowercase();
    let value = |prefix: &str| -> Option<u8> {
        clean
            .strip_prefix(prefix)?
            .trim()
            .trim_end_matches('%')
            .trim()
            .parse::<u8>()
            .ok()
            .filter(|v| *v <= 100)
    };
    if let Some(pct) = value("volume to ").or_else(|| value("set volume to ")) {
        return Some(Op::SetVolume(pct));
    }
    if let Some(pct) = value("brightness to ").or_else(|| value("set brightness to ")) {
        return Some(Op::SetBrightness(pct));
    }
    Some(match clean.as_str() {
        "mute" | "mute audio" | "mute sound" => Op::SetMute(true),
        "unmute" | "unmute audio" | "unmute sound" => Op::SetMute(false),
        "pause music" | "pause media" | "pause playback" => Op::Pause,
        "play music" | "resume music" | "resume playback" => Op::Play,
        "next song" | "next track" => Op::Next,
        "previous song" | "previous track" => Op::Previous,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Probe;
    impl Runner for Probe {
        fn exists(&self, program: &str) -> bool {
            matches!(
                program,
                "wpctl" | "brightnessctl" | "playerctl" | "osascript"
            )
        }
        fn run(&self, program: &str, args: &[&str]) -> Result<String, String> {
            match (program, args) {
                ("wpctl", ["get-volume", _]) => Ok("Volume: 0.30 [MUTED]".into()),
                ("osascript", ["-e", "get volume settings"]) => Ok(
                    "output volume:30, input volume:50, alert volume:90, output muted:true".into(),
                ),
                ("osascript", _) => Ok(String::new()),
                ("brightnessctl", ["get"]) => Ok("30".into()),
                ("brightnessctl", ["max"]) => Ok("100".into()),
                ("playerctl", ["status"]) => Ok("Paused".into()),
                ("playerctl", _) | ("brightnessctl", _) | ("wpctl", _) => Ok(String::new()),
                _ => Err("unsupported".into()),
            }
        }
    }

    #[test]
    fn undo_refuses_external_changes_instead_of_overwriting_them() {
        let probe = Probe;
        assert!(
            Undo::Volume {
                before: 10,
                after: 30
            }
            .verify_current(&probe)
            .is_ok()
        );
        assert!(
            Undo::Volume {
                before: 10,
                after: 80
            }
            .verify_current(&probe)
            .is_err()
        );
        assert!(
            Undo::Mute {
                before: false,
                after: true
            }
            .verify_current(&probe)
            .is_ok()
        );
        assert!(
            Undo::Mute {
                before: false,
                after: false
            }
            .verify_current(&probe)
            .is_err()
        );
        assert!(
            Undo::Brightness {
                before: 10,
                after: 30
            }
            .verify_current(&probe)
            .is_ok()
        );
        assert!(
            Undo::Brightness {
                before: 10,
                after: 80
            }
            .verify_current(&probe)
            .is_err()
        );
    }

    #[test]
    fn exact_phrases_do_not_accept_dictation() {
        assert_eq!(parse("volume to 30%"), Some(Op::SetVolume(30)));
        assert_eq!(parse("set brightness to 70"), Some(Op::SetBrightness(70)));
        assert_eq!(parse("mute"), Some(Op::SetMute(true)));
        assert_eq!(parse("next track"), Some(Op::Next));
        assert_eq!(parse("volume to 101"), None);
        assert_eq!(parse("I listened to music today"), None);
        assert_eq!(parse("Can you delete all my files?"), None);
        assert_eq!(parse("set volume to 50; rm -rf"), None);
    }

    #[test]
    fn readback_and_capability_probe() {
        let p = Probe;
        #[cfg(not(target_os = "macos"))]
        assert_eq!(
            capabilities(&p),
            vec![Capability::Audio, Capability::Brightness, Capability::Media]
        );
        #[cfg(target_os = "macos")]
        assert_eq!(capabilities(&p), vec![Capability::Audio]);
        assert_eq!(volume(&p).unwrap(), (30, true));
        assert_eq!(brightness(&p).unwrap(), 30);
        assert!(execute(Op::SetVolume(30), &p).is_ok());
        assert!(execute(Op::Pause, &p).unwrap().contains("verified"));
        assert!(execute(Op::SetVolume(120), &p).is_err());
    }

    #[test]
    fn parses_coreaudio_settings_without_guessing_missing_fields() {
        assert_eq!(
            percent_from_macos("output volume:30, input volume:50, output muted:true"),
            Ok((30, true))
        );
        assert_eq!(
            percent_from_macos("output volume:0, alert volume:50, output muted:false"),
            Ok((0, false))
        );
        assert!(percent_from_macos("output volume:101, output muted:false").is_err());
        assert!(percent_from_macos("output volume:40").is_err());
        assert!(percent_from_macos("output volume:40, output muted:unknown").is_err());
    }

    #[test]
    fn refuses_invalid_or_nonfinite_audio_data() {
        assert!(percent_from_wpctl("Volume: NaN").is_err());
        assert!(percent_from_wpctl("Volume: 9.99").is_err());
        assert_eq!(
            percent_from_pactl("Volume: front-left: 32768 / 50% / -18dB").unwrap(),
            50
        );
    }
}
