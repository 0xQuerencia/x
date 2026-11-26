# X Jarvis voice acceptance

November 26, 2026. The local branch starts from X main at `48b12ebf`.
X's remote is `xpub` in the nested Hex repository. PRs #231–235 were
already merged. PR #237 covers a separate headless Linux baseline.

## Existing paths reviewed

- Linux finishes ordinary dictation through `DictationOutput::deliver`. It
  routes Smart speech before dictation transformations. Dictate mode and
  unmatched Auto speech continue through the normal processing and paste path.
- The Agent recording entry point obtains a transcript from Hex's local
  `dictation stop --print`. The service routes obvious commands through the
  validated action registry. The existing Decider gets local candidates only
  when command rules cannot settle the request. Work keeps its existing review
  and supervised harness boundary.
- The existing Workflows pane and CLI author, validate, edit, preview and run
  typed steps. A sequential run takes an approved snapshot. A dependent step
  requires verified predecessor completion. Stop and the run deadline apply
  to the owned run. The verified literal text step is bound to a new editor
  opened by that run. It rejects reused windows and protected fields.
- Desktop capability reporting distinguishes a successful read in the current
  session from an advertised operation and an opt-in gate. The diagnostic
  shares the editor readiness check with workflow execution.

## Observed coverage

| Platform | Executed operation and evidence | Remaining acceptance |
| --- | --- | --- |
| Linux, Hyprland 0.57.0 | A finished-transcript fixture entered the actual X Agent voice service. Local rules selected the workspace action with zero classifier calls. The test approved it and verified the effect through the real compositor. Stop discarded a delayed transcript. A separate saved workflow inserted a literal Unicode line into a disposable GTK editor via the private AT-SPI bus and read it back. Reused and protected targets were refused. | Physical shortcut, microphone and installed-artifact acceptance. The editor was a GTK fixture. |
| Linux, KDE Wayland, KWin 6.8.2 | The native adapter reported that it needed opt-in. With session opt-in and `kdotool` 0.3.0, X read the window list and verified a workspace switch after review in a disposable compositor. | Physical Plasma session, remaining window operations and microphone. Verified editor typing reports unsupported. |
| Windows | Existing Win32 capability reporting was reviewed. | Native UI Automation and physical acceptance. Verified editor typing reports unsupported. |
| macOS | Existing input and platform paths were reviewed. | Native Accessibility adapter and physical acceptance. Verified editor typing reports unsupported. |
| Linux, Gemini Live | Synthetic tests covered PCM exchange, interruption, a failed connection, quota, deadline, GoAway and unexpected tool output. An unavailable connection did not affect the local workspace rules. A separate offline run transcribed a synthetic WAV with Gemini disabled. | Provider, speaker and microphone acceptance need their own authorized run. |

## Latency

Warm debug builds on Linux x86-64, AMD Ryzen 7 7435HS. Timings use a
monotonic clock and nearest-rank p50/p95. All audio inputs were synthetic.

| Environment | Stage | Samples | p50 | p95 |
| --- | --- | ---: | ---: | ---: |
| Linux CPU, offline | Ready PCM to Parakeet Unified English (`Q8_0`) transcript | 20 | 142.692 ms | 185.097 ms |
| Hyprland, offline | Release to supplied transcript | 50 | 0.099 ms | 0.143 ms |
| Hyprland, offline | Release to observed intent preview | 50 | 1.181 ms | 1.264 ms |
| Hyprland, offline | Release to observed confirmed workspace outcome | 50 | 78.973 ms | 80.048 ms |
| KDE Wayland, offline | Release to supplied transcript | 50 | 0.106 ms | 0.157 ms |
| KDE Wayland, offline | Release to observed intent preview | 50 | 1.195 ms | 1.327 ms |
| KDE Wayland, offline | Release to observed confirmed workspace outcome | 50 | 78.985 ms | 79.914 ms |
| Linux, simulated cloud protocol | Ready PCM to first decoded response PCM | 50 | 0.006 ms | 0.008 ms |

The offline transcription test starts with prepared PCM and a resident
model. The compositor runs start at simulated release, inject the finished
transcript, poll status about every millisecond and immediately approve the
exact preview. Their numbers exclude recognition and human review time.
The cloud fixture measures local encoding and decoding. The live terminal
command separately reports release to first audio queued for playback when
an authorized connection runs. Live cloud latency, physical first spoken
audio and Windows/macOS percentiles remain unmeasured.

## Reproduce

Run the disposable compositor checks with the required tools on `PATH`.
Omit `X_TEST_VOICE_BENCH_RUNS` for acceptance without timing.

```sh
X_TEST_VOICE_BENCH_RUNS=50 scripts/smoke-x/hyprland-voice.sh
X_TEST_VOICE_BENCH_RUNS=50 scripts/smoke-x/kde-voice.sh
cargo test --locked --bin hex talkback
cargo test --locked --bin hex simulated_talkback_first_audio_latency -- --ignored --nocapture
```

For CPU inference, use an isolated support directory containing the known
model artifact and an eSpeak NG fixture.

```sh
scratch=$(mktemp -d)
mkdir -p "$scratch/support/models"
ln -s "$PARAKEET_MODEL_PATH" "$scratch/support/models/parakeet-unified-en-0.6b-Q8_0.gguf"
espeak-ng -v en-us -s 150 -w "$scratch/workspace-three.wav" 'Go to workspace three.'
HEX_APPLICATION_SUPPORT_DIR="$scratch/support" \
  X_VOICE_BENCH_WAV="$scratch/workspace-three.wav" \
  X_VOICE_BENCH_MODEL=parakeet_unified_en X_VOICE_BENCH_RUNS=20 \
  cargo test --locked --bin hex offline_transcript_latency -- --ignored --test-threads=1 --nocapture
```
