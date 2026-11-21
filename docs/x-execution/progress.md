# X roadmap progress

| PR | State | Branch / PR | Evidence |
| --- | --- | --- | --- |
| P0.1 | Open | [#100](https://github.com/0xQuerencia/x/pull/100) | [Audited source, PR collisions and baseline tests](feature-inventory.md) |
| Baseline parser fix | Open | [#101](https://github.com/0xQuerencia/x/pull/101) | Catalog fixture reproduced the failure. Linux, macOS and Windows x64 CI passed. |
| P0.2 | Open | [#102](https://github.com/0xQuerencia/x/pull/102) | Linux, macOS and Windows x64 CI passed. The read-only Hyprland probe passed. |
| P0.3 | Open | [#103](https://github.com/0xQuerencia/x/pull/103) | Linux, macOS and Windows x64 CI passed, including the isolated Linux Sway and X11 suites. The live read-only Hyprland contract passed. With #101 applied, 386 tests passed and 8 were ignored. |
| P0.4 | Open | [#104](https://github.com/0xQuerencia/x/pull/104) | Linux, macOS and Windows x64 CI passed. With #101 applied, 390 tests passed and 8 were ignored. New tests cover forged metadata, duplicate approval and changed close targets. |
| P0.5 | Open | [#105](https://github.com/0xQuerencia/x/pull/105) | Linux, macOS and Windows x64 CI passed. With #101 applied, 392 tests passed and 8 were ignored. Legacy trust requires fresh approval for protected operations. |
| P0.6 | Open | [#106](https://github.com/0xQuerencia/x/pull/106) | Initial Linux, macOS and Windows x64 CI passed. A follow-up releases the file lock explicitly. All four ledger tests passed at the updated head. |
| P0.7 | Open | [#107](https://github.com/0xQuerencia/x/pull/107) | Linux, macOS and Windows x64 CI passed. With #101 applied, 400 tests passed and 8 were ignored. The cancelled run reaped its owned fake runner, and an independent run completed. |
| P0.8 | Open | [#108](https://github.com/0xQuerencia/x/pull/108) | Linux, macOS and Windows x64 CI passed. With #101 applied, 402 tests passed and 8 were ignored. The full local Linux debug link was stopped, so the native Doctor CLI smoke remains pending. |
| P1.1 | Open | [#109](https://github.com/0xQuerencia/x/pull/109) | Linux, macOS and Windows x64 CI passed. With #101 applied, 405 tests passed and 8 were ignored. Work and workflows use the existing supervised runner through a shared interface. |
| P1.2 | Open | [#110](https://github.com/0xQuerencia/x/pull/110) | Linux, macOS and Windows x64 CI passed before the ledger fix propagated. Live CLI probes recognized OpenCode 2.2.7 and Claude Code 2.1.328. OpenCode login remained unknown after a timeout. |
| P1.3 | Open | [#111](https://github.com/0xQuerencia/x/pull/111) | With #101 and the #106 lock fix applied, 413 x-agent tests passed and 9 were ignored. Router passed 161 tests with 1 ignored. An isolated OpenCode 2.2.7 service returned and selected its exact session. The Linux full-app test compile failure was fixed by reusing its fixture directory. |
| P1.4 | Open | [#112](https://github.com/0xQuerencia/x/pull/112) | 417 x-agent tests passed with #101 and the ledger fix. A real OpenCode 2.2.7 process acknowledged and streamed an approved send through a local provider fixture. |
| P1.5 | Open | [#113](https://github.com/0xQuerencia/x/pull/113) | 421 x-agent tests passed with #101. Tests cover list expiry, ambiguous titles, approval and a closed source window. The native OpenCode service accepted a fixture selection. |
