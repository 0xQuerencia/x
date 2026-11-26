# X GitHub PR inspection (Labs)

With X agent enabled, use `hex agent github-pr owner/repo 7 --account your-login`.
The read-only command uses the official installed `gh` CLI and existing
GitHub authentication. It checks the exact account before and after the PR
read. It accepts only an explicit github.com owner/repository, numeric PR
and expected login. No login or account-switch flow is launched.

The JSON response reports the observed head commit, base branch, PR state,
draft flag, mergeability, review decision, check-run/status metadata and
bounded changed-file statistics. Incomplete file lists and truncated check
lists are marked. An unknown or passing check result does not establish
merge readiness or code correctness. Neither comments, raw patches, PR
body, tokens nor process stderr are emitted by this adapter.

Only fixed `gh api --method GET ... user` and `gh pr view --json ...` commands
are used; no arbitrary endpoint, shell, model access or write operation
exists. Each subprocess has a five-second timeout and the existing bounded
capture limit. Auth, timeout, unavailable repository and rate-limit errors
return a generic refusal without credentials or raw stderr.

Reference: [GitHub CLI pr view](https://cli.github.com/manual/gh_pr_view)
and [GitHub CLI api](https://cli.github.com/manual/gh_api). Mock fixtures
exercise exact command arguments, identity mismatches, partial metadata and
account switches. Actual read against an authorized installed account remains
an acceptance gate. This is F02, not reviewed GitHub writes (F03), a generic
connector registry (F01), or permission to merge.
