use super::{LocalTranscriber, read_wav_for_parakeet, set_device};
use crate::app_settings::TranscriptionDevice;
use crate::transcription::models::{TranscriptionModelId, TranscriptionSelection};
use std::path::PathBuf;
use std::time::Instant;

#[test]
#[ignore = "requires a synthetic WAV and isolated installed GGUF model path"]
fn offline_transcript_latency() {
    let wav = PathBuf::from(std::env::var_os("X_VOICE_BENCH_WAV").expect("set X_VOICE_BENCH_WAV"));
    let model = std::env::var("X_VOICE_BENCH_MODEL").expect("set X_VOICE_BENCH_MODEL");
    let runs: usize = std::env::var("X_VOICE_BENCH_RUNS")
        .unwrap_or_else(|_| "20".into())
        .parse()
        .unwrap();
    assert!((10..=100).contains(&runs));
    assert!(
        std::env::var_os("HEX_APPLICATION_SUPPORT_DIR").is_some(),
        "use an isolated support directory"
    );
    let selection = TranscriptionSelection {
        model: model
            .parse::<TranscriptionModelId>()
            .expect("use a built-in model ID"),
        language: "en".into(),
    };
    set_device(TranscriptionDevice::Cpu);
    let samples = read_wav_for_parakeet(&wav).unwrap();
    let mut transcriber = LocalTranscriber::load(&selection).unwrap();
    let mut durations = Vec::with_capacity(runs);
    for pass in 0..runs + 2 {
        let started = Instant::now();
        let transcript = transcriber.transcribe(&samples).unwrap();
        let elapsed = started.elapsed();
        assert!(
            !transcript.trim().is_empty(),
            "the fixture should contain speech"
        );
        if pass >= 2 {
            durations.push(elapsed);
        }
    }
    durations.sort_unstable();
    let milliseconds = |percentile: usize| {
        durations[(percentile * runs).div_ceil(100) - 1].as_secs_f64() * 1_000.0
    };
    println!(
        "linux offline ready_pcm_to_transcript model={model} device=CPU n={runs} samples={} p50_ms={:.3} p95_ms={:.3}",
        samples.len(),
        milliseconds(50),
        milliseconds(95)
    );
}
