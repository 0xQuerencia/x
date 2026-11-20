# macOS installed application launching (experimental)

X's source builds can discover apps in `/Applications`, `/System/Applications`,
and `~/Applications`, including apps one directory below those roots.
The catalog checks for an actual `.app` bundle with `Contents/Info.plist`;
it never walks into application contents or executes strings from a transcript.

`X, open Visual Studio Code` uses the existing installed-app allowlist and
passes the observed bundle path as an argument to macOS `open -a`, without a
shell. Names must match one installed app uniquely; ambiguous matches are
refused. App discovery is cached for at most 60 seconds.

**Scope:** launching installed apps only. macOS window enumeration, focusing,
workspaces, action verification, native accessibility permissions, and the
complete X agent loop are **not** enabled or validated by this change. Windows
and Linux catalog behavior is unchanged. Real-device validation on macOS is
required before documenting this as supported.
