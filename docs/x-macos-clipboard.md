# X macOS native clipboard (experimental)

X's explicit Copy action uses the native macOS `/usr/bin/pbcopy` to
set the general clipboard, then uses `/usr/bin/pbpaste` once to confirm
the exact requested UTF-8 text (with a bounded retry). It does not read
previous clipboard contents, monitor clipboard changes, keep clipboard
history, or transmit the clipboard to any model.

The request must contain 1–8000 characters, with no NUL, and follows X's
normal action permission policy. X reports success only after readback matches.
No shell, user-supplied executable path or command injection is involved.
Linux continues using wl-copy/wl-paste and Windows continues using the
native clipboard implementation.

**Physical macOS QA pending:** On a disposable Mac test account, copy
ordinary and Unicode text, verify the text in an app, try after manually
changing clipboard contents, and check that failures are surfaced. Verify
X neither observes an unrelated clipboard nor writes text to a different
person's session. The Rust tests cover input bounds, not native Clipboard
services or TCC behavior.
