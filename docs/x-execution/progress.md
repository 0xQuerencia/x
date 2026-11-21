# X roadmap progress

| PR | State | Branch / PR | Evidence |
| --- | --- | --- | --- |
| P0.1 | Open | [#100](https://github.com/0xQuerencia/x/pull/100) | [Audited source, PR collisions and baseline tests](feature-inventory.md) |
| Baseline parser fix | Open | [#101](https://github.com/0xQuerencia/x/pull/101) | Catalog fixture reproduced the failure. With the fix, 381 tests passed and 7 were ignored. |
| P0.2 | Open | [#102](https://github.com/0xQuerencia/x/pull/102) | Doctor tests and read-only Hyprland probe passed. Combined with #101, 383 tests passed and 7 were ignored. |
| P0.3 | Pending | Capability contract fixtures | Depends on P0.2. Reuse the existing fake window controller, workflow host and fake OpenCode executable. |
