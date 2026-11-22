# X on KDE Plasma 6 Wayland — experimental

The X fork has an **experimental** KWin window backend via `kdotool`'s
KWin-script and session D-Bus bridge. It is **not yet a validated KDE
release** and never silently falls back to an XWayland window manager.

## Set up a disposable KDE user session

Install X with the normal Arch source installer from the repository and
install `kdotool` using a trusted, compatible package for your distribution.
Run these commands **in the graphical KDE session**, not over an SSH-only
shell:

```bash
export X_ENABLE_EXPERIMENTAL_KDE=1
x restart
x agent doctor --running --json
```

For a first install, export the flag before running
`bash scripts/install-arch.sh`, which starts the X user service. X now
preserves `X_ENABLE_EXPERIMENTAL_KDE` in its private per-session
`session.env` alongside Wayland display and D-Bus environment variables;
the flag is included on an explicitly requested start/restart. Leaving it
unset writes an empty value and keeps KWin control disabled. Exporting the
flag in a terminal **without restarting X** does not enable an already
running service.

The backend needs the same user and KWin session D-Bus; it must refuse if
that access is unavailable. Doctor's `partial` capability reports mean
source support exists, **not** that an operation was live-verified.

## Safe KDE smoke matrix

Use expendable test windows with no unsaved work. Record the X commit SHA,
Plasma/KWin versions, `kdotool` version, OS, actual result and failure
messages; do not copy private window titles or terminal contents into
issue reports.

1. With the flag **unset**, `x agent doctor` must identify KDE's
   opt-in requirement, without guessing X11 capabilities.
2. With the flag set and service restarted, Doctor should find KWin and
   successfully read a normal window list. If it cannot, treat all
   window operations as unsupported.
3. With two disposable windows, verify exact-window selection, focus,
   maximize/undo, minimize, moving to an existing desktop and changing
   desktop. Confirm the correct window and actual desktop after each action.
4. Close one disposable window *after* selecting it. Any command using
   that stale identity must refuse; it must not act on a different window.
5. Request desktop 11 and verify the capability refuses rather than
   silently remapping it to desktop 10. Pinned/unknown desktop locations
   also must not be misreported.
6. Test a real microphone, Ctrl+Space dictation, Wayland focus changes,
   the approved OpenCode workflow and Stop. Keep the decision model on its
   free tier and never grant permissions automatically to resolve failures.

**Acceptance** requires actual observations from a KDE 6 desktop, not only
the Rust test suite or a CI stub. Until then label KDE experimental.
