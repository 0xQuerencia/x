# X Windows 11 preview builds

X's current Windows 11 port is **experimental**, not a tested release.

A merge of the preview workflow creates a downloadable, unsigned `x.exe`
build on the Windows GitHub Actions runner, with `SHA256SUMS`. Nothing is
deployed or installed automatically.

## Get a preview

1. Open the [X Windows 11 preview workflow](https://github.com/0xQuerencia/x/actions/workflows/x-windows-preview.yml).
2. Open the successful run of the exact `main` commit you intend to test.
3. Under **Artifacts**, download `x-win11-preview-<commit SHA>` (requires
   a GitHub login), extract `x.exe` and `SHA256SUMS`.
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
