# X roadmap progress

| PR | State | Branch / PR | Evidence |
| --- | --- | --- | --- |
| P0.1 | Open | [#100](https://github.com/0xQuerencia/x/pull/100) | [Audited source, PR collisions and baseline tests](feature-inventory.md) |
| Baseline parser fix | Open | [#101](https://github.com/0xQuerencia/x/pull/101) | Catalog fixture reproduced the failure. Linux, macOS and Windows x64 CI passed. |
| P0.2 | Open | [#102](https://github.com/0xQuerencia/x/pull/102) | Linux, macOS and Windows x64 CI passed. The read-only Hyprland probe passed. |
| P0.3 | Open | [#103](https://github.com/0xQuerencia/x/pull/103) | Linux, macOS and Windows x64 CI passed, including the isolated Linux Sway and X11 suites. The live read-only Hyprland contract passed. With #101 applied, 386 tests passed and 8 were ignored. |
| P0.4 | Open | [#104](https://github.com/0xQuerencia/x/pull/104) | Formatting and strict Clippy passed. With #101 applied, 390 tests passed and 8 were ignored. New tests cover forged metadata, duplicate approval and changed close targets. |
| P0.5 | Open | [#105](https://github.com/0xQuerencia/x/pull/105) | Strict x-agent and Linux Hex Clippy passed. With #101 applied, 392 tests passed and 8 were ignored. Legacy trust requires fresh approval for protected operations. |
