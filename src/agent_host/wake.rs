//! Hands-free wake word. Opt-in (`hex agent listen on`), local, and honest about what it
//! does: while on, the microphone is open and each stretch of speech is transcribed on this
//! machine. If it starts with a wake word ("Friday, start coding", or "Friday" and then the
//! command) it becomes an X command, planned and approved exactly like a typed one. Anything
//! else is dropped on the spot: never typed, never logged, never stored, never sent anywhere.
//! It steps aside whenever a dictation is recording or transcribing.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use x_agent::service::Mode;

use crate::control::ControlReply;

/// Speech must be this far above the room's noise floor (dB) to start or continue an
/// utterance. A fixed level would never work: quiet rooms sit near -60 dBFS and noisy
/// microphones near -40.
const SPEECH_OVER_FLOOR_DB: f32 = 10.0;
/// ...and never counts below this absolute level, so digital silence plus a tick is not speech.
const MIN_SPEECH_DB: f32 = -55.0;
/// The floor is this low percentile of the last `FLOOR_WINDOW` chunk levels. Speech is
/// louder than the room, and a sentence fills far less than 80% of a few seconds, so the
/// percentile follows the room and cannot get stuck on a level it learned in a quiet moment.
const FLOOR_PERCENTILE: usize = 20;
const FLOOR_WINDOW: usize = 250;
/// Silence that ends an utterance.
const HANG: Duration = Duration::from_millis(800);
/// Shorter utterances are clicks and coughs.
const MIN_SPEECH: Duration = Duration::from_millis(350);
/// A wake phrase is short; anything longer is a conversation and is cut off here.
const MAX_UTTERANCE: Duration = Duration::from_secs(12);
/// Audio kept from before the level crossed the threshold, so the first word is whole.
const PRE_ROLL: Duration = Duration::from_millis(300);
/// After a bare "Friday", how long the next utterance counts as the command.
const ARMED_FOR: Duration = Duration::from_secs(8);

static ENABLED: AtomicBool = AtomicBool::new(false);
static LOADED: AtomicBool = AtomicBool::new(false);
static STARTED: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);

fn file() -> Option<PathBuf> {
    crate::app_paths::support_dir()
        .ok()
        .map(|dir| dir.join("x-wake-listen"))
}

pub fn enabled() -> bool {
    if !LOADED.swap(true, Ordering::AcqRel) {
        let on = file()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .is_some_and(|text| text.trim() == "on");
        ENABLED.store(on, Ordering::Release);
    }
    ENABLED.load(Ordering::Acquire)
}

pub(super) fn stop() {
    STOP.store(true, Ordering::Release);
}

/// `hex agent listen on|off|status`.
pub(super) fn command(mode: &str) -> ControlReply {
    match mode {
        "status" => ControlReply::done(describe()),
        "on" | "off" => {
            let on = mode == "on";
            if on && !cfg!(target_os = "linux") {
                return ControlReply::error(
                    "unsupported",
                    "hands-free listening is only available on Linux so far",
                );
            }
            let Some(path) = file() else {
                return ControlReply::error("failed", "the data folder is unavailable");
            };
            if let Err(error) = super::write_private(&path, mode.as_bytes()) {
                return ControlReply::error(
                    "failed",
                    format!("couldn't save the setting: {error}"),
                );
            }
            LOADED.store(true, Ordering::Release);
            ENABLED.store(on, Ordering::Release);
            ControlReply::done(describe())
        }
        _ => ControlReply::error("usage", "listen is on, off or status"),
    }
}

fn describe() -> String {
    let words = x_agent::tools::wake_words().join(", ");
    if enabled() {
        format!(
            "Hands-free listening is on: say {words} and a command. The microphone stays open; \
             other speech is dropped, never typed, stored or sent"
        )
    } else {
        format!("Hands-free listening is off (wake words: {words})")
    }
}

// ---- what was said ---------------------------------------------------------------------

#[derive(Debug, Eq, PartialEq)]
pub(super) enum Heard {
    /// Not for X.
    Ignore,
    /// Just the wake word; the next utterance is the command.
    Armed,
    Command(Mode, String),
}

#[derive(Default)]
pub(super) struct Wake {
    armed_until: Option<Instant>,
}

impl Wake {
    pub(super) fn hear(&mut self, text: &str, now: Instant) -> Heard {
        let armed = self.armed_until.take().is_some_and(|until| now < until);
        if let Some(rest) = super::wake_split(text) {
            return match super::split_mode(&rest) {
                Some((mode, command)) => Heard::Command(mode, command),
                None => {
                    self.armed_until = Some(now + ARMED_FOR);
                    Heard::Armed
                }
            };
        }
        if armed && let Some((mode, command)) = super::split_mode(text) {
            return Heard::Command(mode, command);
        }
        Heard::Ignore
    }
}

// ---- cutting the microphone stream into utterances ------------------------------------

fn level_db(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return f32::NEG_INFINITY;
    }
    let mean = samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32;
    10.0 * mean.max(1e-12).log10()
}

#[derive(Default)]
pub(super) struct Segmenter {
    levels: std::collections::VecDeque<f32>,
    pre: Vec<f32>,
    utterance: Vec<f32>,
    active: bool,
    quiet: usize,
    speech: usize,
}

impl Segmenter {
    pub(super) fn reset(&mut self) {
        let levels = std::mem::take(&mut self.levels);
        *self = Self::default();
        self.levels = levels;
    }

    /// Whether `level` is speech: clearly above the room's noise floor, which is a low
    /// percentile of the recent levels.
    fn is_speech(&mut self, level: f32) -> bool {
        self.levels.push_back(level);
        if self.levels.len() > FLOOR_WINDOW {
            self.levels.pop_front();
        }
        let mut sorted: Vec<f32> = self.levels.iter().copied().collect();
        sorted.sort_by(f32::total_cmp);
        let floor = sorted[sorted.len() * FLOOR_PERCENTILE / 100];
        level >= (floor + SPEECH_OVER_FLOOR_DB).max(MIN_SPEECH_DB)
    }

    fn samples(rate: u32, span: Duration) -> usize {
        (f64::from(rate) * span.as_secs_f64()) as usize
    }

    /// Feeds one chunk; returns a finished utterance (at the input rate) once speech ends.
    pub(super) fn push(&mut self, chunk: &[f32], rate: u32) -> Option<Vec<f32>> {
        let loud = self.is_speech(level_db(chunk));
        if !self.active {
            if loud {
                self.active = true;
                self.utterance = std::mem::take(&mut self.pre);
                self.utterance.extend_from_slice(chunk);
                self.speech = chunk.len();
                self.quiet = 0;
            } else {
                self.pre.extend_from_slice(chunk);
                let keep = Self::samples(rate, PRE_ROLL);
                if self.pre.len() > keep {
                    self.pre.drain(..self.pre.len() - keep);
                }
            }
            return None;
        }
        self.utterance.extend_from_slice(chunk);
        if loud {
            self.quiet = 0;
            self.speech += chunk.len();
        } else {
            self.quiet += chunk.len();
        }
        let ended = self.quiet >= Self::samples(rate, HANG)
            || self.utterance.len() >= Self::samples(rate, MAX_UTTERANCE);
        if !ended {
            return None;
        }
        let long_enough = self.speech >= Self::samples(rate, MIN_SPEECH);
        let clip = std::mem::take(&mut self.utterance);
        self.reset();
        long_enough.then_some(clip)
    }
}

// ---- the listener ---------------------------------------------------------------------

/// Starts the listener thread once. It does nothing until `hex agent listen on`.
pub(crate) fn start() {
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    #[cfg(target_os = "linux")]
    if let Err(error) = std::thread::Builder::new()
        .name("x-wake".into())
        .spawn(listen)
    {
        tracing::warn!(%error, "could not start the hands-free listener");
    }
}

#[cfg(target_os = "linux")]
fn listen() {
    use crate::audio::{AudioInput, AudioInputEvent};
    use crate::transcription::engine::{TranscriptionEngine, dictation_busy};

    let mut input: Option<AudioInput> = None;
    let mut segmenter = Segmenter::default();
    let mut wake = Wake::default();
    let mut warned = false;
    let mut last_state = "";
    while !STOP.load(Ordering::Acquire) {
        let (on, labs, busy) = (enabled(), crate::labs::x_agent(), dictation_busy());
        let engine = TranscriptionEngine::shared();
        let state = match (on, labs, busy, engine.is_some()) {
            (false, ..) => "off",
            (_, false, ..) => "waiting: X is off in Labs",
            (_, _, true, _) => "paused: dictation is active",
            (_, _, _, false) => "waiting: the transcription model is not loaded",
            _ => "listening",
        };
        if state != last_state {
            tracing::info!(state, "hands-free listener");
            last_state = state;
        }
        let idle = state != "listening";
        let Some(engine) = engine.filter(|_| !idle) else {
            // Off, busy dictating, or the model is not ready: the microphone is released.
            input = None;
            segmenter.reset();
            std::thread::sleep(Duration::from_millis(500));
            continue;
        };
        let open = match input.take() {
            Some(open) => open,
            None => match AudioInput::open_system_default() {
                Ok(open) => open,
                Err(error) => {
                    if !std::mem::replace(&mut warned, true) {
                        tracing::warn!(%error, "hands-free listening could not open the microphone");
                    }
                    std::thread::sleep(Duration::from_secs(5));
                    continue;
                }
            },
        };
        match open.recv_timeout(Duration::from_millis(100)) {
            AudioInputEvent::Chunk { samples, .. } => {
                let rate = open.sample_rate;
                input = Some(open);
                if let Some(clip) = segmenter.push(&samples, rate) {
                    transcribe_and_act(&engine, clip, rate, &mut wake);
                }
            }
            AudioInputEvent::Timeout => input = Some(open),
            AudioInputEvent::StreamFailed(error) => {
                tracing::warn!(%error, "hands-free microphone stream failed; reopening");
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn transcribe_and_act(
    engine: &std::sync::Arc<crate::transcription::engine::TranscriptionEngine>,
    clip: Vec<f32>,
    rate: u32,
    wake: &mut Wake,
) {
    let clip = crate::dictation::resample_for_parakeet(&clip, rate);
    let ms = clip.len() / 16;
    let vad = crate::transcription::speech_detector::contains_speech(&clip);
    // Logged but not used as a gate: the wake word is the filter, and the voice detector
    // rejected real speech on some microphones.
    tracing::info!(ms, ?vad, "hands-free utterance");
    let Some(selection) = engine.held_selection() else {
        return;
    };
    let text = engine
        .foreground(
            clip,
            Box::new(move |model, samples| {
                let transcriber = crate::transcription::engine::held(model, &selection)
                    .ok_or_else(|| "the transcription model changed".to_string())?;
                crate::transcription::engine::transcribe_clip(transcriber, samples)
                    .map_err(|error| format!("{error:#}"))
            }),
        )
        .map_err(|error| format!("{error:#}"))
        .and_then(|pending| pending.wait())
        .map(|transcribed| transcribed.text);
    let text = match text {
        Ok(text) => text,
        Err(error) => {
            tracing::info!(%error, "hands-free transcription failed");
            return;
        }
    };
    // Only what starts with a wake word survives this point. The text itself is never logged.
    let heard = wake.hear(&text, Instant::now());
    tracing::info!(
        chars = text.chars().count(),
        outcome = match &heard {
            Heard::Ignore => "no wake word",
            Heard::Armed => "wake word, waiting for the command",
            Heard::Command(..) => "wake word and command",
        },
        "hands-free transcribed"
    );
    match heard {
        Heard::Ignore => {}
        Heard::Armed => super::service().emit("info", "Listening: say the command"),
        Heard::Command(mode, command) => {
            super::service().emit("info", &format!("Heard: {command}"));
            super::plan_spoken(mode, &command);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;

    fn tone(ms: u64, amplitude: f32) -> Vec<f32> {
        (0..RATE as u64 * ms / 1000)
            .map(|i| amplitude * ((i as f32) * 0.3).sin())
            .collect()
    }

    fn feed(seg: &mut Segmenter, audio: &[f32]) -> Vec<Vec<f32>> {
        audio
            .chunks(RATE as usize / 50)
            .filter_map(|chunk| seg.push(chunk, RATE))
            .collect()
    }

    #[test]
    fn speech_followed_by_silence_is_one_utterance_with_a_pre_roll() {
        let mut seg = Segmenter::default();
        let mut audio = tone(500, 0.0);
        audio.extend(tone(900, 0.3));
        audio.extend(tone(1200, 0.0));
        let found = feed(&mut seg, &audio);
        assert_eq!(found.len(), 1);
        let secs = found[0].len() as f64 / f64::from(RATE);
        assert!((1.5..2.3).contains(&secs), "{secs}");
    }

    /// A deterministic noise-like signal at roughly `amplitude`.
    fn noise(ms: u64, amplitude: f32, seed: u32) -> Vec<f32> {
        let mut x = seed;
        (0..RATE as u64 * ms / 1000)
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                amplitude * ((x >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0)
            })
            .collect()
    }

    #[test]
    fn a_noisy_microphone_does_not_look_like_endless_speech() {
        // About -40 dBFS of hiss, like the microphone this was built on.
        let hiss = |ms, seed| noise(ms, 0.017, seed);
        let mut seg = Segmenter::default();
        assert!(
            feed(&mut seg, &hiss(15_000, 1)).is_empty(),
            "hiss is not speech"
        );
        let mut audio = hiss(500, 2);
        audio.extend(noise(900, 0.2, 3)); // speech about 20 dB above the hiss
        audio.extend(hiss(1500, 4));
        let found = feed(&mut seg, &audio);
        assert_eq!(found.len(), 1, "speech over steady hiss is one utterance");
    }

    #[test]
    fn a_room_that_gets_louder_never_sticks_on_speech() {
        // Starts nearly silent, then the room (fan, music) settles at -40 dBFS for good.
        let mut seg = Segmenter::default();
        let mut audio = noise(300, 0.0005, 5);
        audio.extend(noise(40_000, 0.017, 6));
        let found = feed(&mut seg, &audio);
        assert!(
            found.len() <= 1 && found.iter().all(|c| c.len() < RATE as usize * 12),
            "at most one false start, never a stuck 12 s utterance: {}",
            found.len()
        );
        // And real speech on top of the new room level is still caught.
        let mut more = noise(500, 0.017, 7);
        more.extend(noise(900, 0.2, 8));
        more.extend(noise(1500, 0.017, 9));
        assert_eq!(feed(&mut seg, &more).len(), 1);
    }

    #[test]
    fn a_click_and_plain_silence_produce_nothing() {
        let mut seg = Segmenter::default();
        let mut audio = tone(300, 0.0);
        audio.extend(tone(60, 0.5));
        audio.extend(tone(1500, 0.0));
        assert!(feed(&mut seg, &audio).is_empty());
        assert!(feed(&mut seg, &tone(3000, 0.0)).is_empty());
    }

    #[test]
    fn endless_speech_is_cut_at_the_limit() {
        let mut seg = Segmenter::default();
        let mut audio = tone(500, 0.0);
        audio.extend(tone(13_000, 0.3));
        let found = feed(&mut seg, &audio);
        assert_eq!(found.len(), 1);
        assert!(found[0].len() as u64 <= RATE as u64 * 12 + RATE as u64 / 10);
    }

    fn at(secs: u64) -> Instant {
        static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *BASE.get_or_init(Instant::now) + Duration::from_secs(secs)
    }

    #[test]
    fn a_wake_word_with_a_command_runs_it_and_anything_else_is_ignored() {
        let mut wake = Wake::default();
        assert_eq!(
            wake.hear("Friday, start coding", at(0)),
            Heard::Command(Mode::Actions, "start coding".into())
        );
        assert_eq!(
            wake.hear("hey friday work add tests", at(1)),
            Heard::Command(Mode::Work, "add tests".into())
        );
        assert_eq!(wake.hear("what time is the meeting", at(2)), Heard::Ignore);
        assert_eq!(wake.hear("I said friday earlier", at(3)), Heard::Ignore);
    }

    #[test]
    fn a_bare_wake_word_takes_the_next_utterance_once_and_only_briefly() {
        let mut wake = Wake::default();
        assert_eq!(wake.hear("Friday", at(0)), Heard::Armed);
        assert_eq!(
            wake.hear("open firefox", at(3)),
            Heard::Command(Mode::Actions, "open firefox".into())
        );
        assert_eq!(wake.hear("open firefox", at(4)), Heard::Ignore, "only once");
        assert_eq!(wake.hear("Friday", at(10)), Heard::Armed);
        assert_eq!(
            wake.hear("open firefox", at(30)),
            Heard::Ignore,
            "the window closed"
        );
    }
}
