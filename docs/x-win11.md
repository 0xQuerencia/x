# X on Windows 11 — experimental native backend

The **X fork** does not yet have a Windows installer or a Windows release asset.
The upstream Hex Windows installer is a different product/build and must not be
presented as an X release.

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
| Screenshots, system control, process management, wake listener | **Not at Linux parity** |

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
2. Open Notepad, Firefox, File Explorer and two windows of the same app. Test
   list, focus, maximize/undo, minimize/undo, and graceful close. Verify the
   intended HWND/PID only; ensure unsaved Notepad prompts are respected.
3. Close a window after it was listed, then repeat its stale action. X must
   refuse rather than operate a reused HWND.
4. Ask to move a window to desktop 2 or switch to desktop 2. X must explicitly
   say this is unsupported; it must not invoke a different or guessed desktop.
5. Test dictation, screenshot, workflows, app launching, OpenCode, accessibility
   grants, undo, failed-network decisions and context editing separately. Mark
   every unsupported behavior as **unsupported**, not pass.
6. Confirm no private microphone/audio transcript, key, screenshot or selected
   text enters telemetry or a release log. Verify Windows permissions and
   program signature/hash, packaged executable, onboarding and update path.

**Release rule:** a cross-compile or CI check is not a Windows live test.
Do not tag X as Windows-ready or publish an official X Windows release
until the relevant real-device matrix and packaged artifact are validated.
