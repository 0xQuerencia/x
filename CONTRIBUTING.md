# Contributing to X

X is a fork of Hex focused on a local, validated voice and desktop assistant.

**Pull requests to this fork are welcome.** Branch from `main` and open a
focused PR against `0xQuerencia/x:main`. Describe the behavior change,
relevant tests, failure modes and any OS/compositor requirements. Do not
combine unrelated features just to reduce the PR count.

Before requesting merge, run:

```sh
cargo fmt --all -- --check
cargo clippy --locked -p x-agent --all-targets -- -D warnings
cargo test --locked -p x-agent --all-targets
```

Changes to the native Hex frontend also need the relevant upstream Hex tests
and UI goldens. A passing CI build is **not** evidence of a live Hyprland,
KDE Plasma, X11, microphone or accessibility test. Run the appropriate smoke
test on a disposable session and record the result honestly.

Preserve backward compatibility for saved workflow definitions where possible.
Do not add model-generated commands, bypass permission checks or silently
make an opt-in feature automatic. The shared backend/controller enforces
fresh target identity, verified effects and bounded Undo.

Read [the X release quality gate](docs/x-release-qa.md) before tagging or
publishing. Releases and AUR publication require owner approval.

This file describes the **X fork**. The upstream project may use a different
contribution policy; refer to its own repository for upstream submissions.
