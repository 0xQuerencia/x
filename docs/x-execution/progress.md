# X roadmap progress

| PR | State | Branch / PR | Evidence |
| --- | --- | --- | --- |
| P0.1 | Open | [#100](https://github.com/0xQuerencia/x/pull/100) | [Audited source, PR collisions and baseline tests](feature-inventory.md) |
| Baseline parser fix | Open | [#101](https://github.com/0xQuerencia/x/pull/101) | Catalog fixture reproduced the failure. Linux, macOS and Windows x64 CI passed. |
| P0.2 | Open | [#102](https://github.com/0xQuerencia/x/pull/102) | Linux, macOS and Windows x64 CI passed. The read-only Hyprland probe passed. |
| P0.3 | Open | [#103](https://github.com/0xQuerencia/x/pull/103) | Linux, macOS and Windows x64 CI passed, including the isolated Linux Sway and X11 suites. The live read-only Hyprland contract passed. With #101 applied, 386 tests passed and 8 were ignored. |
| P0.4 | Open | [#104](https://github.com/0xQuerencia/x/pull/104) | Linux, macOS and Windows x64 CI passed. With #101 applied, 390 tests passed and 8 were ignored. New tests cover forged metadata, duplicate approval and changed close targets. |
| P0.5 | Open | [#105](https://github.com/0xQuerencia/x/pull/105) | Linux, macOS and Windows x64 CI passed. With #101 applied, 392 tests passed and 8 were ignored. Legacy trust requires fresh approval for protected operations. |
| P0.6 | Open | [#106](https://github.com/0xQuerencia/x/pull/106) | Linux, macOS and Windows x64 CI passed. With #101 applied, 397 tests passed and 8 were ignored. Tests cover private records, legacy import, restart recovery and failed audit writes. |
| P0.7 | Open | [#107](https://github.com/0xQuerencia/x/pull/107) | Linux, macOS and Windows x64 CI passed. With #101 applied, 400 tests passed and 8 were ignored. The cancelled run reaped its owned fake runner, and an independent run completed. |
| P0.8 | Open | [#108](https://github.com/0xQuerencia/x/pull/108) | Formatting and strict x-agent Clippy passed. With #101 applied, 402 tests passed and 8 were ignored. Doctor exposes capability evidence and probe source. |
