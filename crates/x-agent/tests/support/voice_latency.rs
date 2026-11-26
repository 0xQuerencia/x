use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use x_agent::hexcli::{Stt, SttError};
use x_agent::phase::Phase;
use x_agent::service::{ActionCompletion, Mode, RealLauncher, Service};

pub fn run(platform: &str, workspace: impl Fn() -> u64) {
    let Ok(runs) = std::env::var("X_TEST_VOICE_BENCH_RUNS") else {
        return;
    };
    let runs: usize = runs
        .parse()
        .expect("X_TEST_VOICE_BENCH_RUNS must be an integer");
    assert!((10..=200).contains(&runs), "Use 10-200 samples");

    struct ScriptedSpeech {
        count: AtomicUsize,
        delivered: Sender<Instant>,
        words: [&'static str; 2],
    }
    impl Stt for ScriptedSpeech {
        fn start(&self) -> Result<(), SttError> {
            Ok(())
        }
        fn stop_print(&self) -> Result<String, SttError> {
            let transcript =
                self.words[self.count.fetch_add(1, Ordering::Relaxed) % self.words.len()].into();
            self.delivered.send(Instant::now()).unwrap();
            Ok(transcript)
        }
        fn cancel(&self) -> Result<(), SttError> {
            Ok(())
        }
    }

    let (delivered, transcript) = channel();
    let (words, expected) = if workspace() == 2 {
        (["go to workspace three", "go to workspace two"], [3, 2])
    } else {
        (["go to workspace two", "go to workspace three"], [2, 3])
    };
    let service = Service::new(
        Arc::new(ScriptedSpeech {
            count: AtomicUsize::new(0),
            delivered,
            words,
        }),
        Arc::new(RealLauncher),
        None,
    );
    let mut to_transcript = Vec::with_capacity(runs);
    let mut to_intent = Vec::with_capacity(runs);
    let mut to_confirmed = Vec::with_capacity(runs);
    let wait = |phase| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = service.snapshot(u64::MAX);
            if snapshot.phase == phase {
                return snapshot;
            }
            assert!(
                !matches!(snapshot.phase, Phase::Failed | Phase::Cancelled)
                    && Instant::now() < deadline,
                "Expected {phase:?}, got {snapshot:?}"
            );
            thread::sleep(Duration::from_millis(1));
        }
    };
    for sample in 0..runs + 2 {
        service.voice_start(Mode::Actions).unwrap();
        let released = Instant::now();
        service.voice_stop().unwrap();
        let delivered = transcript.recv_timeout(Duration::from_secs(2)).unwrap();
        let preview = wait(Phase::AwaitingApproval).task.unwrap();
        let intent = released.elapsed();
        service.approve(preview.id).unwrap();
        let snapshot = wait(Phase::Completed);
        let confirmed = released.elapsed();
        assert_eq!(snapshot.action_outcome, Some(ActionCompletion::Confirmed));
        assert_eq!(workspace(), expected[sample % 2]);
        if sample >= 2 {
            to_transcript.push(delivered.duration_since(released));
            to_intent.push(intent);
            to_confirmed.push(confirmed);
        }
    }
    for (stage, mut values) in [
        ("finished_fixture_transcript", to_transcript),
        ("intent", to_intent),
        ("verified_completion", to_confirmed),
    ] {
        values.sort_unstable();
        let milliseconds = |percentile: usize| {
            values[(percentile * runs).div_ceil(100) - 1].as_secs_f64() * 1_000.0
        };
        println!(
            "{platform} offline from_release {stage} n={runs} p50_ms={:.3} p95_ms={:.3}",
            milliseconds(50),
            milliseconds(95)
        );
    }
}
