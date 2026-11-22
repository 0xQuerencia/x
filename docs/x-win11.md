# X on Windows 11 — experimental native backend

The X fork publishes an unsigned, portable `x.exe` as an experimental GitHub
prerelease. See the [Windows preview instructions](x-win11-preview.md) for
downloads and checksum verification. X has no stable Windows installer. The
upstream Hex Windows installer belongs to Hex.

The native backend is built into Windows source builds. It uses Win32 APIs,
not PowerShell, shell commands or text from speech.

| X desktop action | Windows 11 state |
| --- | --- |
| List visible top-level windows | Implemented; requires physical smoke |
| Focus a window | Implemented; subject to Windows foreground policy |
| Maximize, minimize and Undo | Implemented; requires physical smoke |
| Close a window | Graceful WM_CLOSE, never force-terminate |
| Choose between matching windows | Existing stable-id/PID controller; requires smoke |
| Numbered virtual desktops and moving between them | **Not implemented** (no fake desktop numbers) |
| Installed Start Menu app launch, default URL opening, folders | Implemented; physical smoke needed |
| Speech, Actions and saved workflows | Require separate Windows physical end-to-end verification |
| Explicit Unicode typing and clipboard copy | Native SendInput / CF_UNICODETEXT implementations; physical smoke needed |
| Browser tab shortcuts (new/close/reopen) | Native fixed-key shortcuts after verifying browser focus; physical smoke needed |
| Next/previous media track | Fixed Windows media keys (unverified delivery); Play/Pause refuse toggle ambiguity |
| Explicit full-screen screenshot | Bounded GDI capture to a local BMP, no automatic capture, uploads or window/region fallback; Windows physical QA pending |
| Window/region screenshots, system volume/mute/brightness, process management, wake listener | **Not at Linux parity** |

## Developer build on a Windows 11 machine

Install Visual Studio Build Tools with Desktop C++, the MSVC Rust toolchain,
CMake, Ninja and a Vulkan-capable driver. Follow the documented upstream
Windows build prerequisites in `docs/windows.md`.

```powershell
git clone https://github.com/0xQuerencia/x.git
cd x
cargo fmt --all -- --check
cargo clippy --locked -p x-agent --all-targets -- -D warnings
cargo test --locked -p x-agent --all-targets
cargo build --locked --release --bin hex
.\target\release\hex.exe agent doctor
```

## Physical acceptance before any Windows release

Use a disposable Windows 11 user profile with a real microphone. Record the
commit SHA, machine/OS build, actual result and whether the action was verified.

1. Launch X without interfering with any installed Hex version, record shortcut
   and local transcription behavior, and verify the service lifecycle.
2. Switch from X to a keyboard-intensive app (including a Tauri 2 app where
   possible), press/release the dictation hotkey, switch back, and repeat.
   Check that the shortcut never disappears, duplicates, or remains held.
   This foreground-hook regression requires a **physical Windows desktop**.
3. Open Notepad, Firefox, File Explorer and two windows of the same app. Test
   list, focus, maximize/undo, minimize/undo, and graceful close. Verify the
   intended HWND/PID only; ensure unsaved Notepad prompts are respected.
4. Close a window after it was listed, then repeat its stale action. X must
   refuse rather than operate a reused HWND.
5. Ask to move a window to desktop 2 or switch to desktop 2. X must explicitly
   say this is unsupported; it must not invoke a different or guessed desktop.
6. Test dictation, screenshot, workflows, app launching, OpenCode, accessibility
   grants, undo, failed-network decisions and context editing separately. Mark
   every unsupported behavior as **unsupported**, not pass.
7. Confirm no private microphone/audio transcript, key, screenshot or selected
   text enters telemetry or a release log. Verify Windows permissions and
   program signature/hash, packaged executable, onboarding and update path.

**Release rule:** a cross-compile or CI check is not a Windows live test.
Do not tag X as Windows-ready or publish an official X Windows release
until the relevant real-device matrix and packaged artifact are validated.

## Verified focus after launching apps

The X Actions path can now wait for a newly started app window to become the
foreground Win32 window **only when its HWND identity and process ID match
three consecutive observations**. This supports a cautious launch-then-type
sequence on Windows without Linux's `/proc` or `hyprctl`. The subsequent Windows Unicode
typing path also rejects a changed HWND or PID **immediately before each
character**, rather than trusting only the prior focus wait. An app that delegates
to a different process, a stolen focus, unavailable window enumeration, or a
slow startup fails closed: X refuses to type rather than guessing which window
should receive the text. This is not yet evidence of live Windows 11 typing QA.

## Windows screenshot smoke test

Run the explicit `hex agent screenshot screen` command (or speak the exact screenshot phrase) from a disposable Windows 11 user account. Confirm the resulting `X-screenshot-*.bmp` opens correctly, represents the real virtual desktop and is saved only in the user Pictures directory. Window and region screenshot requests must refuse instead of broadening to the entire display. Test locked/protected desktops and multi-monitor sizing without bypassing security surfaces. The screenshot is never automatically forwarded to a model or coding harness. Physical results still need to be recorded.
