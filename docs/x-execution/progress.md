# X roadmap progress

**Historical record.** See the [remaining roadmap ledger](remaining-roadmap.md)
for live integration and acceptance evidence. The
[master-plan status](master-plan-status.md) preserves the source audit for PR #220.
The dated integration states and evidence below remain unchanged.

Integration snapshot: 2026-11-23. PRs #82, #93–#132, #140, #141,
#144, #149, #153–#174 and #177 are merged. New #178–#183 are open
development work and must not be counted until their checks pass and they
merge. Individual PRs retain historical CI evidence.
This is source integration progress, not physical-platform acceptance.
**Merged source is not real-device acceptance.** Linux, macOS, and Windows
CI cannot verify microphone, accessibility, install, signing or release parity.

| PR | State | Branch / PR | Evidence |
| --- | --- | --- | --- |
| P0.1 | Merged source | [#100](https://github.com/0xQuerencia/x/pull/100) | [Audited source, PR collisions and baseline tests](feature-inventory.md) |
| Baseline parser fix | Merged source | [#101](https://github.com/0xQuerencia/x/pull/101) | Catalog fixture reproduced the failure. Linux, macOS and Windows x64 CI passed. |
| P0.2 | Merged source | [#102](https://github.com/0xQuerencia/x/pull/102) | Linux, macOS and Windows x64 CI passed. The read-only Hyprland probe passed. |
| P0.3 | Merged source | [#103](https://github.com/0xQuerencia/x/pull/103) | Linux, macOS and Windows x64 CI passed, including the isolated Linux Sway and X11 suites. The live read-only Hyprland contract passed. With #101 applied, 386 tests passed and 8 were ignored. |
| P0.4 | Merged source | [#104](https://github.com/0xQuerencia/x/pull/104) | Linux, macOS and Windows x64 CI passed. With #101 applied, 390 tests passed and 8 were ignored. New tests cover forged metadata, duplicate approval and changed close targets. |
| P0.5 | Merged source | [#105](https://github.com/0xQuerencia/x/pull/105) | Linux, macOS and Windows x64 CI passed. With #101 applied, 392 tests passed and 8 were ignored. Legacy trust requires fresh approval for protected operations. |
| P0.6 | Merged source | [#106](https://github.com/0xQuerencia/x/pull/106) | Linux, macOS and Windows x64 CI passed at the updated head. A follow-up releases the file lock explicitly, and all four ledger tests passed. |
| P0.7 | Merged source | [#107](https://github.com/0xQuerencia/x/pull/107) | Linux, macOS and Windows x64 CI passed. With #101 applied, 400 tests passed and 8 were ignored. The cancelled run reaped its owned fake runner, and an independent run completed. |
| P0.8 | Merged source | [#108](https://github.com/0xQuerencia/x/pull/108) | Linux, macOS and Windows x64 CI passed. With #101 applied, 402 tests passed and 8 were ignored. The full local Linux debug link was stopped, so the native Doctor CLI smoke remains pending. |
| P1.1 | Merged source | [#109](https://github.com/0xQuerencia/x/pull/109) | Linux, macOS and Windows x64 CI passed. With #101 applied, 405 tests passed and 8 were ignored. Work and workflows use the existing supervised runner through a shared interface. |
| P1.2 | Merged source | [#110](https://github.com/0xQuerencia/x/pull/110) | Linux, macOS and Windows x64 CI passed at the updated head. Live CLI probes recognized OpenCode 2.2.7 and Claude Code 2.1.328. OpenCode login remained unknown after a timeout. |
| P1.3 | Merged source | [#111](https://github.com/0xQuerencia/x/pull/111) | With #101 and the #106 lock fix applied, 413 x-agent tests passed and 9 were ignored. Router passed 161 tests with 1 ignored. An isolated OpenCode 2.2.7 service returned and selected its exact session. The Linux full-app test compile failure was fixed by reusing its fixture directory. Linux, macOS and Windows x64 CI then passed. |
| P1.4 | Merged source | [#112](https://github.com/0xQuerencia/x/pull/112) | Linux, macOS and Windows x64 CI passed. With #101 and the ledger fix, 417 x-agent tests passed. A real OpenCode 2.2.7 process acknowledged and streamed an approved send through a local provider fixture. |
| P1.5 | Merged source | [#113](https://github.com/0xQuerencia/x/pull/113) | Linux, macOS and Windows x64 CI passed. With #101, 421 x-agent tests passed and 9 were ignored. Tests cover list expiry, ambiguous titles, approval and a closed source window. The native OpenCode service accepted a fixture selection. |
| P1.6 | Merged source | [#114](https://github.com/0xQuerencia/x/pull/114) | Linux, macOS and Windows x64 CI passed. With #101, 424 x-agent tests passed and 9 were ignored. Claude Code 2.1.328 started and resumed the same project session through a local provider fixture with an empty tools list. |
| P1.7 | Merged source | [#115](https://github.com/0xQuerencia/x/pull/115) | Linux, macOS and Windows x64 CI passed. With #101, 427 x-agent tests passed and 9 were ignored. A supplied transcript reached an actual OpenCode 2.2.7 session through a local provider. Microphone capture is pending. |
| P2.1 | Merged source | [#141](https://github.com/0xQuerencia/x/pull/141) | The native Hyprland 0.57.0 window contract passed on CachyOS. The test verified move, workspace switch, maximize, minimize, undo, close and stale identity refusal. Two separate live GTK accessibility tests passed. Installation, microphone and full release acceptance remain pending. |
| P2.2–P2.12 | Merged backend fixes; physical QA pending | [#93](https://github.com/0xQuerencia/x/pull/93), [#94](https://github.com/0xQuerencia/x/pull/94), [#95](https://github.com/0xQuerencia/x/pull/95), [#97](https://github.com/0xQuerencia/x/pull/97), [#98](https://github.com/0xQuerencia/x/pull/98), [#99](https://github.com/0xQuerencia/x/pull/99) | Pending fixes cover selected backend operations. Isolated Sway and X11 CI passed in #103. Physical KDE, Windows and macOS acceptance remains pending. |
| P3.1 | Merged source | [#116](https://github.com/0xQuerencia/x/pull/116) | Linux, macOS and Windows x64 CI passed. With #101, 433 x-agent tests passed and 9 were ignored. Tests exercised real linked Git worktrees and refused hidden paths and symlink escapes. |
| P3.2 | Merged source | [#117](https://github.com/0xQuerencia/x/pull/117) | Linux, macOS and Windows x64 CI passed. Covers aliases, ambiguity, restart and stale paths. |
| P3.3 | Merged source | [#118](https://github.com/0xQuerencia/x/pull/118) | Linux, macOS and Windows x64 CI passed. Session links are unverified after restart until an applicable provider lookup. |
| P3.4 | Merged foundation; acceptance partial | [#119](https://github.com/0xQuerencia/x/pull/119) | Linux, macOS and Windows x64 CI passed for the CLI project-selection and alias commands. Dedicated project voice routing is pending. |
| P3.5 | Merged explicit handoffs; acceptance partial | [#120](https://github.com/0xQuerencia/x/pull/120), [#154](https://github.com/0xQuerencia/x/pull/154), [#158](https://github.com/0xQuerencia/x/pull/158), [#159](https://github.com/0xQuerencia/x/pull/159) | Typed project-file lines and owner-supplied browser URL/title metadata reach only a selected coding session after Work approval, with bounded redaction. Automatic browser-tab and screenshot content capture is still unwired. | |
| P4.1 | Merged source | [#121](https://github.com/0xQuerencia/x/pull/121) | Linux, macOS and Windows x64 CI passed for graph validation and legacy workflow migration. |
| P4.2 | Merged source | [#122](https://github.com/0xQuerencia/x/pull/122) | Linux, macOS and Windows x64 CI passed for bounded scheduling, dependency gating and Stop. |
| P4.3 | Merged source | [#123](https://github.com/0xQuerencia/x/pull/123) | Linux, macOS and Windows x64 CI passed. Local Git fixtures preserve dirty primary checkouts. |
| P4.4 | Merged foundation; acceptance partial | [#124](https://github.com/0xQuerencia/x/pull/124) | Linux, macOS and Windows x64 CI passed for the journal and retries gated by reconciliation. A live restart/reconnect workflow remains to be exercised. |
| P4.5 | Merged foundation; acceptance partial | [#126](https://github.com/0xQuerencia/x/pull/126), [#127](https://github.com/0xQuerencia/x/pull/127) | Linux, macOS and Windows x64 CI passed. Parallel isolated work and dependency failure are covered with two fixture adapters and real Git worktrees. [#140](https://github.com/0xQuerencia/x/pull/140) also ran installed OpenCode and Claude Code with local providers and verified delivery of the dependent job after both predecessors. |
| P4.6 | Merged source; live QA pending | [#128](https://github.com/0xQuerencia/x/pull/128), [#161](https://github.com/0xQuerencia/x/pull/161) | Exact worktree reattachment and one-use owner approval now permit only a clean Git fast-forward after review. Linux, macOS and Windows x64 CI passed; live merge-back and recovery testing remain. |
| P5.1–P5.2 | Merged via #132/#140; operator QA pending | [#132](https://github.com/0xQuerencia/x/pull/132), [#133–#139](https://github.com/0xQuerencia/x/pull/139) | Scoped receipts, revocation, status and a permission preview are open. The intermediate heads failed Linux formatting and macOS/Windows Clippy. [#140](https://github.com/0xQuerencia/x/pull/140) wires native approval to the executor. Linux, macOS and Windows x64 CI passed on that head. With #101 applied, the local suite passed 501 tests with 9 ignored. An initial local build hit a linker bus error and the full local core suite passed on retry using a temporary target. |
| P5.3 | Merged source | [#125](https://github.com/0xQuerencia/x/pull/125) | Linux, macOS and Windows x64 CI passed for typed delivery and Git evidence results. |
| P5.4 | Merged test execution; live QA pending | [#130](https://github.com/0xQuerencia/x/pull/130), [#156](https://github.com/0xQuerencia/x/pull/156), [#160](https://github.com/0xQuerencia/x/pull/160) | Fixed test suites now enter native explicit owner approval, run under bounded worktree grants and expose a Rune Work-pane review control. Isolated Git worktrees do **not** sandbox repository code against the user's OS and network. | |
| P5.5 | Merged foundation; acceptance partial | [#129](https://github.com/0xQuerencia/x/pull/129) | Linux, macOS and Windows x64 CI passed for stale context refusal. Live window, accessibility and optional-screen verification remain pending. |
| P5.6 | Merged foundation; acceptance partial | [#140](https://github.com/0xQuerencia/x/pull/140) | Coding graphs enter the normal approval flow and consume one-use job grants. Tests cover Stop and stale approval. The approved test runner, operator-facing retries and restart reconciliation remain pending. |

## Additional merged source and release safety (22 November 2026)

| Area | Merged PRs | Verified behavior | Remaining gate |
| --- | --- | --- | --- |
| Arch installation and rollback | [#153](https://github.com/0xQuerencia/x/pull/153), [#155](https://github.com/0xQuerencia/x/pull/155) | Checked ownership, atomically staged active binary and prior rollback copy; isolated fresh-install/upgrade smoke | Real user clean-install, active process upgrade and rollback |
| Privacy of coding context | [#154](https://github.com/0xQuerencia/x/pull/154), [#158](https://github.com/0xQuerencia/x/pull/158), [#159](https://github.com/0xQuerencia/x/pull/159) | Bounded project-file excerpts and manually supplied browser metadata, redaction and selected-session Work approval | Real permissioned browser-tab and screen capture, privacy review |
| Bounded project tests | [#156](https://github.com/0xQuerencia/x/pull/156), [#160](https://github.com/0xQuerencia/x/pull/160) | Fixed suites, exact grants, Stop, retained isolated Git worktrees, Rune review | Real operator execution and verification evidence |
| Windows unsigned preview packaging and focus | [#157](https://github.com/0xQuerencia/x/pull/157), [#163](https://github.com/0xQuerencia/x/pull/163) | Isolated X-only install/uninstall in Windows CI; Win32 launch-to-type rechecks foreground HWND and PID. Cross-platform CI passed. | Real Windows 11 setup, microphone, accessibility, process handoff and input QA |
| KDE correctness and service startup | [#168](https://github.com/0xQuerencia/x/pull/168), [#169](https://github.com/0xQuerencia/x/pull/169) | Refuses unsupported KWin desktop numbers; carries the explicit opt-in flag into the graphical X user service. Cross-platform CI passed. | Real Plasma 6 with kdotool, microphone, windows and undo |
| Windows local desktop actions | [#165](https://github.com/0xQuerencia/x/pull/165), [#166](https://github.com/0xQuerencia/x/pull/166) | Native bounded next/previous media keys; explicit GDI full-screen BMP captures. All three OS CI suites passed. | Windows 11 physical test of media, screenshot privacy and OS permissions |
| Coding recovery and Rune inspection | [#167](https://github.com/0xQuerencia/x/pull/167), [#170](https://github.com/0xQuerencia/x/pull/170) | Read-only persisted claim status plus a Rune Work-pane summary, without automatic retries or private provider IDs. Cross-platform CI passed. | Provider-confirmed restart/reconciliation and user-facing retry approval |
| Physical release acceptance gate | [#172](https://github.com/0xQuerencia/x/pull/172) | Privacy-minimal, exact-commit, per-platform operator checklist that refuses any missing, failed or unsupported case. Linux/Windows schema tests passed. | Actually perform the test matrix on real machines and sign/distribute stable artifacts |
| Build provenance | [#162](https://github.com/0xQuerencia/x/pull/162) | SHA256, compiler, target and exact commit in Linux/Windows `BUILD_INFO.json` and tests against tampering | Signing, trusted-source validation and physical release QA |
| Arch helper ownership | [#171](https://github.com/0xQuerencia/x/pull/171) | Installer refuses linked or unrelated agent-desktop directories and commands; isolated installation safety CI passed | Physical fresh install and upgrade on another user's account |
| macOS arm64 bare preview | [#173](https://github.com/0xQuerencia/x/pull/173) | Unsigned arm64 CLI build and checksum/provenance CI; no upstream Hex identity or release changes | Separate signed/notarized app packaging and physical Apple Silicon QA |
| macOS screenshot and clipboard | [#174](https://github.com/0xQuerencia/x/pull/174), [#177](https://github.com/0xQuerencia/x/pull/177) | User-requested local screenshot handling, native clipboard write and readback; cross-platform CI succeeded | macOS privacy permissions, app-level clipboard and screen QA |


All merged CI checks are **source/build evidence**, not live-desktop evidence.
The preview tool has not passed the full cross-platform acceptance matrix.
No signed or stable X release is justified yet.

## Open implementation PRs — not yet delivered

| PR | Work | Dependency / outstanding gate |
| --- | --- | --- |
| [#178](https://github.com/0xQuerencia/x/pull/178) | Owner-selected terminal output as untrusted Work context | Linux, Windows and macOS CI and actual selected-session handoff |
| [#179](https://github.com/0xQuerencia/x/pull/179) | Paged registered-project picker in Rune | Cross-platform CI and real UI keyboard/screenshot checks |
| [#180](https://github.com/0xQuerencia/x/pull/180) | Rune URL/title handoff to selected OpenCode session | Depends on #179; CI and real approval review |
| [#181](https://github.com/0xQuerencia/x/pull/181) | More natural registered-project voice phrases | Cross-platform CI and actual microphone acceptance |
| [#182](https://github.com/0xQuerencia/x/pull/182) | Experimental isolated X Preview.app ZIP for Apple Silicon | CI, real Mac and signing/notarization before stable release |
| [#183](https://github.com/0xQuerencia/x/pull/183) | Exact-identity OpenCode session picker in Rune | Depends on #180 and #179; provider and UI QA |

A numbered PR is not a roadmap percentage. Core P0/P1 source work is merged,
P2–P5 have material implementations with live and integration gaps, and P6–P8
still have substantial UI, integration and release-acceptance work. Do not
claim an exact source-completion percentage without an individually weighted
acceptance inventory.

## Continuation checks on November 23, 2026

These PRs are open for review. The checks below apply to their individual
branches or named preview artifacts.

| Phase | PR | Verified | Still open |
| --- | --- | --- | --- |
| P4.4 / P5.6 | [#185](https://github.com/0xQuerencia/x/pull/185) | The workflow executor writes structured delivery evidence before completion. Tests reopen the journal, preserve acknowledgements on failure and refuse replay or stale settlement. Linux, macOS and Windows compile CI passed. | Provider-side reconciliation when receipt is unknown, approved retries and an actual mid-run disconnect or restart. |
| P8 | [#186](https://github.com/0xQuerencia/x/pull/186) | The published Linux preview at `143fcb480489` crashed on startup on a Ryzen 7 7435HS without AVX-512. The exact unsigned CI artifact at `d23780d5e99e068cb9d4e80376238c46864e1499` passed checksum, provenance and service lifecycle checks on the same machine. | Distribution and physical release acceptance for the fixed artifact. |
| P3.5 / P7.1 | [#187](https://github.com/0xQuerencia/x/pull/187) | In isolated Chromium and Openbox, X refused ungranted selection capture, then reviewed and sent a redacted excerpt to a real OpenCode session using a loopback provider. The provider received neither the fixture credential nor the requested dummy skill's contents. | Automatic tab metadata, console and screenshot handoff. Additional browser and native OS coverage. |
| P6 | [#188](https://github.com/0xQuerencia/x/pull/188) | In the native Linux app under Xvfb, the Commands controls fit at narrow widths. At 860 × 560, Work approval was visible and scrolling reached project tests and recovery. Escape discarded the preview. | Native Windows and macOS Rune acceptance, plus checks on the open project and session picker PRs. |

The unsigned Windows and macOS previews at `143fcb480489` passed downloaded
checksum, provenance and executable-format checks on Linux. The physical
Windows and macOS install, microphone, permissions, accessibility and rollback
checks still need those systems.

## Unfinished work across the full P0–P8 roadmap

The table above tracks integrated PRs, not full roadmap completion.
Until these are verified on the actual target systems, do not label X stable.

| Phase | Outstanding delivery and acceptance |
| --- | --- |
| P0–P1 | Clean-host Doctor, real microphone, 20 spoken commands, real provider failure and Stop tests |
| P2 | Physical KDE, macOS and Windows 11 acceptance; Windows virtual desktops, window/region screenshots, verified media state, native process controls and macOS window parity |
| P3 | Existing exact project voice selection needs additional natural-language coverage; live permissioned browser-tab/screen context and privacy acceptance remain |
| P4 | Live Git merge-back; provider-confirmed restart recovery and explicit operator retries (read-only claims now wired) |
| P5 | Live approved worktree tests and actual desktop/accessibility result readback |
| P6 | Remaining Rune operator UX and keyboard-accessibility/screenshot acceptance (recovery status and tests are now surfaced) |
| P7 | Owner-provided terminal excerpt handoff remains in review; live browser/terminal context integration and advanced spoken interaction remain |
| P8 | Real installation/upgrade/rollback, signatures and notarization, privacy audit and physical multi-platform release acceptance; an unsigned macOS arm64 preview exists, with an experimental .app ZIP under review |

See [release acceptance](../x-release-qa.md). A preview executable is not
a verified release, and a code-only test does not substitute for a physical
desktop check.
