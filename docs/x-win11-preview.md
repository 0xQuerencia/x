# X Windows 11 preview builds

X's current Windows 11 port is **experimental**, not a tested release.

After the preview workflow passes on `main`, it publishes an unsigned `x.exe`
and `SHA256SUMS` as an explicitly labeled GitHub **prerelease**. Nothing is
installed, auto-updated, or advertised as fully supported.

## Get a preview

1. Open [X Releases](https://github.com/0xQuerencia/x/releases) and select the newest **Windows 11 experimental preview** prerelease.
2. Download `x.exe` and `SHA256SUMS` from **Assets**. Do not confuse it with the Linux-only alpha.
3. Verify the checksum before executing.
4. On Windows 11 PowerShell, run:

   ```powershell
   (Get-FileHash .\x.exe -Algorithm SHA256).Hash.ToLowerInvariant()
   Get-Content .\SHA256SUMS
   ```

   The hash must match exactly. An unsigned executable may trigger SmartScreen:
   **do not bypass a warning if you cannot verify its origin and checksum**.
5. Test from an ordinary, disposable Windows user account rather than an
   administrator account or a profile containing important work.

The preview has no installer, signing, automatic updates or product-level
Windows assurance. `X_BUILD=1`, `HEX_LOCAL_BUILD=1`, and
`HEX_TEST_BUILD=1` isolate its app identity and disable release updates.

**Do not share this as a completed Windows release.** Before publishing a
versioned release, complete the real-machine tests in `docs/x-win11.md`,
including microphone, focus, app selection, clipboard, screenshots,
accessibility, workflows, Stop and undo. Unsupported actions must remain
explicitly unsupported.

This is a direct `x.exe` binary; no `setup.exe` or MSI has been verified.
