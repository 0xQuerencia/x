# X macOS local screenshots (experimental)

X can save **user-requested** screenshots on macOS without a cloud request,
a model prompt, a third-party program or an X service screenshot history.

From an authenticated graphical macOS account, run:

```sh
hex agent screenshot screen
hex agent screenshot screen --output "$HOME/Pictures/x-example.png"
```

X invokes Apple's fixed `/usr/sbin/screencapture` command to capture the
**main display only** (noninteractive and with no shutter sound). macOS may
ask for **Screen Recording** permission in System Settings. X must not
attempt to bypass that permission. If the user denies it, no screenshot is
delivered. On a multi-display Mac, the other displays are not implicitly
captured.

The screenshot is temporarily held in an X-owned, mode-0700 support folder.
It must be a PNG of at most 24 MiB. X copies it to a **new**, mode-0600
file in Pictures (or an explicitly chosen PNG destination), then removes
the X temporary file. It never overwrites an existing destination, writes
to the clipboard, sends the screenshot to a model or keeps a capture log.
The utility is terminated on timeout. The private visual Decider feature
remains limited to Linux/Hyprland.

**Unsupported:** screenshot window and screenshot region; these refuse,
rather than unexpectedly taking a full-screen image. Accessibility-driven
window controls and cross-app content access need separate permissions.

## Physical macOS verification

The Rust macOS unit tests cover path bounds, staged-file cleanup and
PNG validation; they are **not** evidence of a successful screen capture.
On a disposable Mac account with X installed, verify a capture after granting
Screen Recording, a refusal after revoking it, an already-existing destination,
a locked screen, a protected app and a Retina/multi-display setup. Confirm
no private file or image bytes enter X logs or remote services. Record the
X commit and exact macOS version before considering this release-ready.
