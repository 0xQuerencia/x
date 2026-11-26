//! Linux terminal push-to-talk. The microphone is opened on Space press and
//! closed on release. No other audio source or local command can enter a turn.

mod live;

use std::io::{self, IsTerminal, Write};
use std::num::{NonZeroU16, NonZeroU32};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TrySendError, sync_channel};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableFocusChange, EnableFocusChange, Event, KeyCode, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::{execute, terminal};
use rodio::buffer::SamplesBuffer;
use x_agent::cancellation::Cancellation;

use crate::audio::{AudioInput, AudioInputEvent};

const KEY: &str = "X_GEMINI_LIVE_API_KEY";
const POLL: Duration = Duration::from_millis(15);
const CONSENT: &str = "Experimental Gemini Live sends the audio you record while holding Space to Google when you release it. Unpaid input and responses may be used to improve Google's products and reviewed by people. Keep this turn free of sensitive, confidential, and personal information. See https://aistudio.google.com/docs/terms#unpaid-services and https://aistudio.google.com/docs/rate-limits. Your project controls quota and billing. Continue for this terminal session? [y/N]";

pub(super) fn run(model: &str) -> u8 {
    match open(model) {
        Ok(()) => 0,
        Err(message) => {
            eprintln!("{message}");
            super::super::EXIT_REFUSED
        }
    }
}

fn open(model: &str) -> Result<(), &'static str> {
    let feature = crate::labs::Feature::GeminiTalkback;
    let enabled = feature.managed().is_none()
        && crate::app_settings::AppSettings::load().is_ok_and(|settings| feature.on(&settings));
    if !enabled {
        return Err("Gemini Live talkback is off. Enable it in Labs to use this command");
    }
    if !live::model_valid(model) {
        return Err("Enter a Gemini Live model ID such as gemini-3.8-live");
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("Talkback needs a visible interactive terminal");
    }
    println!("{CONSENT}");
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|_| "Could not read the talkback consent prompt")?;
    if !answer.trim().eq_ignore_ascii_case("y") {
        return Ok(());
    }
    let key: Arc<str> = std::env::var(KEY)
        .ok()
        .filter(|key| live::key_valid(key))
        .ok_or("Set X_GEMINI_LIVE_API_KEY for your own project in this terminal")?
        .into();
    let _terminal = Terminal::open()?;
    status(
        "Hold Space to record. Release to send. Space interrupts playback, Escape cancels, Q exits.",
    );
    interactive(model, &key)
        .map_err(|_| "Talkback terminal input stopped. You can use the local dictation shortcut")
}

struct Terminal;

impl Terminal {
    fn open() -> Result<Self, &'static str> {
        if !terminal::supports_keyboard_enhancement().unwrap_or(false) {
            return Err("Talkback needs a terminal that reports key release events");
        }
        terminal::enable_raw_mode().map_err(|_| "Could not open talkback terminal input")?;
        let guard = Self;
        execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                    | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
            ),
            EnableFocusChange
        )
        .map_err(|_| "Could not enable talkback key release and focus reporting")?;
        Ok(guard)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            DisableFocusChange,
            PopKeyboardEnhancementFlags
        );
        let _ = terminal::disable_raw_mode();
    }
}

fn status(message: &str) {
    let _ = write!(io::stdout(), "{message}\r\n");
    let _ = io::stdout().flush();
}

struct Recording {
    source: AudioInput,
    started: Instant,
    samples: Vec<f32>,
    max_samples: usize,
}

impl Recording {
    fn open() -> Result<Self, &'static str> {
        let source =
            AudioInput::open_system_default().map_err(|_| "The microphone is unavailable")?;
        let max_samples =
            usize::try_from(u64::from(source.sample_rate) * live::CLIP_LIMIT.as_secs())
                .map_err(|_| "Unsupported microphone sample rate")?;
        if !(8_000..=192_000).contains(&source.sample_rate) {
            return Err("Unsupported microphone sample rate");
        }
        Ok(Self {
            source,
            max_samples,
            samples: Vec::new(),
            started: Instant::now(),
        })
    }

    fn poll(&mut self) -> Result<(), &'static str> {
        if self.started.elapsed() >= live::CLIP_LIMIT {
            return Err(
                "Ten-second recording limit reached. This turn was discarded. Release Space and try again",
            );
        }
        for _ in 0..16 {
            match self.source.recv_timeout(Duration::ZERO) {
                AudioInputEvent::Chunk { samples, .. } => {
                    if samples.len() > self.max_samples - self.samples.len() {
                        return Err("Recording limit reached. This turn was discarded");
                    }
                    self.samples.extend(samples);
                }
                AudioInputEvent::Timeout => break,
                AudioInputEvent::StreamFailed(_) => {
                    return Err("The microphone stopped. This turn was discarded");
                }
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<Vec<u8>, live::Fault> {
        let rate = self.source.sample_rate;
        drop(self.source);
        to_pcm(&self.samples, rate)
    }
}

fn to_pcm(samples: &[f32], rate: u32) -> Result<Vec<u8>, live::Fault> {
    let samples = crate::dictation::try_resample_for_parakeet(samples, rate)
        .map_err(|_| live::Fault::NoAudio)?;
    let max = live::INPUT_RATE as usize * live::CLIP_LIMIT.as_secs() as usize;
    if samples.len() < 320 || samples.len() > max {
        return Err(live::Fault::NoAudio);
    }
    Ok(samples
        .into_iter()
        .flat_map(|sample| {
            ((sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16).to_le_bytes()
        })
        .collect())
}

enum Reply {
    Audio(Vec<i16>),
    End(Result<(), live::Fault>),
}

struct Turn {
    stop: Cancellation,
    replies: Receiver<Reply>,
    worker: JoinHandle<()>,
    released: Instant,
}

impl Turn {
    fn start(model: &str, key: &Arc<str>, pcm: Vec<u8>, released: Instant) -> io::Result<Self> {
        let model = model.to_owned();
        let key = Arc::clone(key);
        let stop = Cancellation::default();
        let worker_stop = stop.clone();
        let (sender, replies) = sync_channel(16);
        let worker = thread::Builder::new()
            .name("x-gemini-live".into())
            .spawn(move || {
                let result = live::turn(&key, &model, &pcm, &worker_stop, |audio| {
                    if worker_stop.requested() {
                        return Err(live::Fault::Cancelled);
                    }
                    sender
                        .try_send(Reply::Audio(audio))
                        .map_err(|error| match error {
                            TrySendError::Disconnected(_) => live::Fault::Cancelled,
                            TrySendError::Full(_) => live::Fault::Unavailable,
                        })
                });
                let _ = sender.try_send(Reply::End(result));
            })?;
        Ok(Self {
            stop,
            replies,
            worker,
            released,
        })
    }
}

struct Playback {
    player: rodio::Player,
    _sink: rodio::MixerDeviceSink,
}

impl Playback {
    fn open() -> Result<Self, live::Fault> {
        let mut sink =
            rodio::DeviceSinkBuilder::open_default_sink().map_err(|_| live::Fault::Unavailable)?;
        sink.log_on_drop(false);
        Ok(Self {
            player: rodio::Player::connect_new(sink.mixer()),
            _sink: sink,
        })
    }

    fn append(&self, audio: Vec<i16>) {
        let samples: Vec<_> = audio
            .into_iter()
            .map(|sample| f32::from(sample) / 32768.0)
            .collect();
        self.player.append(SamplesBuffer::new(
            NonZeroU16::MIN,
            NonZeroU32::new(live::OUTPUT_RATE).unwrap(),
            samples,
        ));
    }
}

fn drain(turn: &Turn, playback: &mut Option<Playback>) -> Option<Result<(), live::Fault>> {
    for _ in 0..16 {
        match turn.replies.try_recv() {
            Ok(Reply::Audio(audio)) => {
                if let Some(playback) = playback {
                    playback.append(audio);
                } else {
                    let player = match Playback::open() {
                        Ok(player) => player,
                        Err(fault) => return Some(Err(fault)),
                    };
                    player.append(audio);
                    let elapsed = turn.released.elapsed().as_millis();
                    status(&format!(
                        "Speaking · first audio queued {elapsed} ms after release"
                    ));
                    *playback = Some(player);
                }
            }
            Ok(Reply::End(result)) => return Some(result),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Some(Err(live::Fault::Unavailable));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => break,
        }
    }
    None
}

fn stop_playing(
    turn: &mut Option<Turn>,
    playback: &mut Option<Playback>,
    retired: &mut Vec<JoinHandle<()>>,
) {
    playback.take();
    if let Some(turn) = turn.take() {
        turn.stop.request();
        retired.push(turn.worker);
    }
}

fn interactive(model: &str, key: &Arc<str>) -> io::Result<()> {
    let mut recording: Option<Recording> = None;
    let mut turn: Option<Turn> = None;
    let mut playback: Option<Playback> = None;
    let mut retired = Vec::new();
    let result = (|| loop {
        retired.retain(|worker: &JoinHandle<()>| !worker.is_finished());
        if event::poll(POLL)? {
            match event::read()? {
                Event::Key(key_event) if key_event.kind == KeyEventKind::Press => {
                    match (key_event.code, key_event.modifiers) {
                        (KeyCode::Char('q'), KeyModifiers::NONE)
                        | (KeyCode::Char('c'), KeyModifiers::CONTROL) => break Ok(()),
                        (KeyCode::Esc, _) => {
                            recording.take();
                            stop_playing(&mut turn, &mut playback, &mut retired);
                            status("Talkback cancelled");
                        }
                        (KeyCode::Char(' '), KeyModifiers::NONE) if recording.is_none() => {
                            stop_playing(&mut turn, &mut playback, &mut retired);
                            if retired.len() >= 2 {
                                status(
                                    "The previous connection is still closing. Try again in a moment",
                                );
                                continue;
                            }
                            status("Recording locally. Release Space to send to Google");
                            match Recording::open() {
                                Ok(opened) => recording = Some(opened),
                                Err(message) => status(message),
                            }
                        }
                        _ => {}
                    }
                }
                Event::Key(event)
                    if event.code == KeyCode::Char(' ') && event.kind == KeyEventKind::Release =>
                {
                    if let Some(recording) = recording.take() {
                        let released = Instant::now();
                        if released.duration_since(recording.started) >= live::CLIP_LIMIT {
                            status("Ten-second recording limit reached. This turn was discarded");
                            continue;
                        }
                        match recording.finish() {
                            Ok(pcm) => match Turn::start(model, key, pcm, released) {
                                Ok(current) => {
                                    status("Microphone off. Sending this turn to Google");
                                    turn = Some(current);
                                }
                                Err(_) => status(live::Fault::Unavailable.message()),
                            },
                            Err(fault) => status(fault.message()),
                        }
                    }
                }
                Event::FocusLost => {
                    recording.take();
                    stop_playing(&mut turn, &mut playback, &mut retired);
                    status("Talkback paused because the terminal lost focus");
                }
                _ => {}
            }
        }
        if let Some(active) = recording.as_mut()
            && let Err(message) = active.poll()
        {
            recording.take();
            status(message);
        }
        if let Some(active) = turn.as_ref()
            && let Some(result) = drain(active, &mut playback)
        {
            if let Err(fault) = result {
                stop_playing(&mut turn, &mut playback, &mut retired);
                status(fault.message());
            } else if let Some(finished) = turn.take() {
                retired.push(finished.worker);
            }
        }
        if turn.is_none() && playback.as_ref().is_some_and(|audio| audio.player.empty()) {
            playback.take();
            status("Ready. Hold Space for a new turn");
        }
    })();
    stop_playing(&mut turn, &mut playback, &mut retired);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_keep_talkback_disconnected() {
        assert!(
            !crate::app_settings::AppSettings::default()
                .labs
                .gemini_talkback
        );
    }

    #[test]
    fn synthetic_48khz_audio_becomes_bounded_16khz_pcm() {
        let samples = vec![0.25; 48_000];
        let audio = to_pcm(&samples, 48_000).unwrap();
        assert_eq!(audio.len(), 32_000);
        assert_eq!(to_pcm(&[], 16_000), Err(live::Fault::NoAudio));
    }
}
