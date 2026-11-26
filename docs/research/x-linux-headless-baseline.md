# X Linux headless performance and local traffic

Measured November 26, 2026 on CachyOS, Linux 7.3.1, on an AMD Ryzen 7 7435HS
with AVX2 and without AVX-512. The binaries were unsigned Ubuntu 24.04 X previews.
Both SHA-256 files and both source and platform manifests verified locally.
The old candidate is [#226's Linux preview](https://github.com/0xQuerencia/x/actions/runs/45728460651)
at `e155d2129ca25bc949989aec8d81d58587e01c72`. The newer candidate is
[#228's Linux preview](https://github.com/0xQuerencia/x/actions/runs/45732994508)
at `053117aa86f675ce8690fb1c0f5c82663bf74f92`, the source tree subsequently
merged as `bd6a58461371999be8c99d179bc9e5802f0c0db1`. The manifests establish
source and artifact integrity within those workflow runs. Publisher signing
and independent attestation were unavailable for this baseline.

| Operation | Samples per candidate | #226 median / p95 | #228 median / p95 |
|---|---:|---:|---:|
| Fresh hidden process to first CLI status | 20 | 190.33 / 233.32 ms | 194.49 / 219.64 ms |
| Warm CLI status | 100 | 11.49 / 12.76 ms | 11.74 / 14.45 ms |
| Warm CLI local plan preview | 100 | 12.88 / 14.96 ms | 13.47 / 19.50 ms |

Fresh process measurements include CLI startup and run with reusable system
filesystem caches. The preview measures a recognized `list windows` text
command to the exact Awaiting Approval response. The script then stops that
specific task outside the timing window. No preview executes a desktop
action. The process used a disposable home and application directory with no
provider installation, settings, physical audio, or desktop session. Other
local builds were active. The single pair establishes observed ranges for
this run. Changes to product performance need repeat measurements under
controlled load.

| Linux service inventory | #226 | #228 |
|---|---:|---:|
| Idle CPU over 8 seconds | 0.625% | 0.500% |
| Initial / warm / after stress RSS | 139,248 / 146,440 / 147,496 KiB | 139,292 / 145,392 / 147,428 KiB |
| Threads at every observation | 35 | 35 |
| Descriptors at every observation | 35 | 35 |

Stress includes 500 status requests and 300 exact preview/Stop pairs after
warmup. In a separate trial #228's RSS fell from 146,600 to 109,908 KiB over
the same sequence. RSS samples include allocator and system residency changes.
Leak detector and ownership tests remain necessary for individual paths.

A separate `strace -ff -e trace=%network` run followed the hidden service,
its children, and CLI processes through startup, status, the local preview,
its exact Stop, and quit. The newer candidate made one IP bind and all 15
observed IP connect attempts targeted the same `127.0.0.1` port. Five observed
connects used Unix sockets. The trace captured zero destination-bearing IP
send syscalls and zero connections to non-loopback addresses. The service
has an explicit [loopback Local API bind](../../src/local_api.rs).
Provider traffic, network Decider, model downloads, the updater, and GUI
operations were outside this trace.

## Repeat the headless measurement

After verifying a downloaded workflow artifact's source and hashes, run:

```sh
python3 scripts/smoke-x/bench-headless-linux.py /absolute/path/to/x-linux-x64
python3 scripts/smoke-x/bench-headless-linux.py /absolute/path/to/x-linux-x64 --network-trace
```

The trace mode uses `strace`. Keeping it separate prevents the tracer's
overhead from affecting the timings. File paths and raw traces stay in the
temporary directory, and the runner emits only timings, resource counts and
socket endpoint summaries. It closes the process and removes the temporary
directory.

Desktop HUD, speech-to-text, actual effect verification, context discovery,
and native Work with an installed provider still need measurements on the
supported desktop sessions. Signing and exact cross-platform installed
acceptance remain part of the release gate.
