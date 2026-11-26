use super::*;
use std::collections::VecDeque;

#[derive(Default)]
struct Fixture {
    sent: Vec<Value>,
    replies: VecDeque<Value>,
}

impl Session for Fixture {
    fn send(&mut self, request: Value) -> Result<(), Fault> {
        self.sent.push(request);
        Ok(())
    }

    fn receive(&mut self) -> Result<Option<Value>, Fault> {
        self.replies.pop_front().map(Some).ok_or(Fault::Rollover)
    }
}

fn fixture() -> Fixture {
    Fixture {
        replies: [json!({"setupComplete": {}})].into(),
        ..Default::default()
    }
}

fn audio(encoded: &str) -> Value {
    json!({"serverContent":{"modelTurn":{"parts":[{"inlineData":{"mimeType":"audio/pcm;rate=24000","data":encoded}}]}}})
}

fn run(
    session: &mut Fixture,
    stop: &Cancellation,
    play: impl FnMut(Vec<i16>) -> Result<(), Fault>,
) -> Result<(), Fault> {
    exchange(
        session,
        "gemini-3.8-live",
        &[0, 0, 15, 0],
        stop,
        Instant::now() + Duration::from_secs(1),
        play,
    )
}

#[test]
fn push_to_talk_sends_only_pcm_and_receives_synthetic_audio() {
    let mut session = fixture();
    session.replies.extend([
        audio("AQD//w=="),
        json!({"serverContent":{"turnComplete":true}}),
    ]);
    let mut played = vec![];
    run(&mut session, &Cancellation::default(), |pcm| {
        played.extend(pcm);
        Ok(())
    })
    .unwrap();
    assert_eq!(played, [1, -1]);
    assert_eq!(session.sent[0]["setup"]["model"], "models/gemini-3.8-live");
    assert!(session.sent[0]["setup"].get("tools").is_none());
    assert!(session.sent[0]["setup"].get("sessionResumption").is_none());
    assert!(
        session.sent[1]["realtimeInput"]
            .get("activityStart")
            .is_some()
    );
    assert_eq!(
        session.sent[2]["realtimeInput"]["audio"]["mimeType"],
        "audio/pcm;rate=16000"
    );
    assert!(
        session.sent[3]["realtimeInput"]
            .get("activityEnd")
            .is_some()
    );
    assert_eq!(session.sent.len(), 4);
}

#[test]
fn rollover_quota_tool_requests_and_malformed_audio_all_stop_without_replay() {
    for (reply, expected) in [
        (json!({"goAway":{"timeLeft":"30s"}}), Fault::Rollover),
        (
            json!({"error":{"status":"RESOURCE_EXHAUSTED","message":"private server detail"}}),
            Fault::Quota,
        ),
        (json!({"toolCall":{"functionCalls":[]}}), Fault::Protocol),
        (audio("bad!"), Fault::Protocol),
    ] {
        let mut session = fixture();
        session.replies.push_back(reply);
        assert_eq!(
            run(&mut session, &Cancellation::default(), |_| panic!(
                "no audio"
            )),
            Err(expected)
        );
        assert_eq!(session.sent.len(), 4);
    }
}

#[test]
fn expired_and_cancelled_turns_never_send_or_replay_audio() {
    let stop = Cancellation::default();
    let mut session = fixture();
    assert_eq!(
        exchange(
            &mut session,
            "gemini-3.8-live",
            &[0, 0],
            &stop,
            Instant::now(),
            |_| panic!()
        ),
        Err(Fault::Deadline)
    );
    assert!(session.sent.is_empty());
    stop.request();
    assert_eq!(
        run(&mut session, &stop, |_| panic!()),
        Err(Fault::Cancelled)
    );
    assert!(session.sent.is_empty());
    let stop = Cancellation::default();
    session
        .replies
        .extend([audio("AQD//w=="), audio("AQD//w==")]);
    let mut plays = 0;
    assert_eq!(
        run(&mut session, &stop, |_| {
            plays += 1;
            stop.request();
            Ok(())
        }),
        Err(Fault::Cancelled)
    );
    assert_eq!(plays, 1);
}

#[test]
fn unavailable_talkback_leaves_the_local_workspace_rules_available() {
    let mut session = Fixture::default();
    assert_eq!(
        run(&mut session, &Cancellation::default(), |_| panic!(
            "no audio"
        )),
        Err(Fault::Rollover)
    );
    assert_eq!(
        session.sent.len(),
        1,
        "setup failed before a clip could be sent"
    );
    assert!(matches!(
        x_agent::tools::plan_rules("go to workspace two").as_deref(),
        Some([x_agent::tools::Action::Window {
            op: x_agent::wm::Op::Workspace,
            ..
        }])
    ));
}

#[test]
#[ignore = "microbenchmark of the in-memory protocol fixture, without provider or playback latency"]
fn simulated_talkback_first_audio_latency() {
    let mut durations = Vec::new();
    for _ in 0..50 {
        let mut session = fixture();
        session.replies.extend([
            audio("AQD//w=="),
            json!({"serverContent":{"turnComplete":true}}),
        ]);
        let started = Instant::now();
        run(&mut session, &Cancellation::default(), |pcm| {
            assert_eq!(pcm, [1, -1]);
            durations.push(started.elapsed());
            Ok(())
        })
        .unwrap();
    }
    durations.sort_unstable();
    let ms = |percentile: usize| {
        durations[(percentile * durations.len()).div_ceil(100) - 1].as_secs_f64() * 1_000.0
    };
    println!(
        "linux cloud_protocol_simulated ready_pcm_to_first_decoded_audio n=50 p50_ms={:.3} p95_ms={:.3}",
        ms(50),
        ms(95)
    );
}
