//! F02: explicit read-only inspection through the installed official GitHub CLI.
//! No arbitrary endpoint, shell, raw patch, model invocation or write authority.

use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

const LIMIT: Duration = Duration::from_secs(5);
const MAX_CHECKS: usize = 100;
const MAX_FILES: usize = 50;
const FIELDS: &str = "number,url,title,state,isDraft,headRefOid,baseRefName,mergeable,reviewDecision,statusCheckRollup,changedFiles,files,additions,deletions";

#[derive(Serialize)]
pub struct Report {
    pub account: String,
    pub repository: String,
    pub number: u32,
    pub url: String,
    pub title: String,
    pub state: String,
    pub draft: bool,
    pub head_commit: String,
    pub base: String,
    pub mergeability: String,
    pub review_decision: String,
    pub checks: Vec<Check>,
    pub reported_checks: usize,
    pub checks_truncated: bool,
    pub files: Vec<FileChange>,
    pub changed_files: u32,
    pub file_details_complete: bool,
    pub additions: u64,
    pub deletions: u64,
    pub observed_at: u64,
    pub read_only: bool,
    pub note: &'static str,
}

#[derive(Serialize)]
pub struct Check {
    pub kind: String,
    pub name: String,
    pub status: String,
    pub conclusion: String,
}

#[derive(Deserialize, Serialize)]
pub struct FileChange {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Deserialize)]
struct Login {
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pull {
    number: u32,
    url: String,
    title: String,
    state: String,
    is_draft: bool,
    head_ref_oid: String,
    base_ref_name: String,
    mergeable: String,
    review_decision: String,
    status_check_rollup: Vec<Value>,
    changed_files: u32,
    files: Vec<FileChange>,
    additions: u64,
    deletions: u64,
}

fn login_ok(login: &str) -> bool {
    (1..=39).contains(&login.len())
        && !login.starts_with('-')
        && !login.ends_with('-')
        && login
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn repository_ok(repo: &str) -> bool {
    let Some((owner, name)) = repo.split_once('/') else {
        return false;
    };
    login_ok(owner)
        && (1..=100).contains(&name.len())
        && !matches!(name, "." | "..")
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn display(text: &str, limit: usize) -> String {
    text.chars()
        .filter(|ch| {
            !ch.is_control() && !matches!(*ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .take(limit)
        .collect()
}

trait Reader {
    fn run(&self, args: &[&str]) -> Result<Vec<u8>, String>;
}

struct Gh;

impl Reader for Gh {
    fn run(&self, args: &[&str]) -> Result<Vec<u8>, String> {
        let mut command = Command::new("gh");
        command
            .args(args)
            .env("GH_HOST", "github.com")
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env("GH_NO_EXTENSION_UPDATE_NOTIFIER", "1")
            .env("GH_PAGER", "cat")
            .env("NO_COLOR", "1")
            .env_remove("GH_DEBUG")
            .env_remove("GH_FORCE_TTY")
            .env_remove("GH_REPO");
        // Use the person's installed CLI and its existing credential storage.
        // Never read a token, run a login flow, or echo process stderr.
        let (ok, output) = crate::desktop::capture(&mut command, LIMIT)
            .map_err(|_| "GitHub CLI is missing, timed out, or returned oversized output")?;
        if !ok {
            return Err(
                "GitHub read failed: check account authentication, repository access, network or API rate limit".into(),
            );
        }
        Ok(output)
    }
}

fn account(reader: &dyn Reader, expected: &str) -> Result<String, String> {
    let bytes = reader.run(&["api", "--hostname", "github.com", "--method", "GET", "user"])?;
    let login: Login =
        serde_json::from_slice(&bytes).map_err(|_| "GitHub did not report an account identity")?;
    if !login_ok(&login.login) || !login.login.eq_ignore_ascii_case(expected) {
        return Err("The authenticated GitHub account does not match the requested account".into());
    }
    Ok(login.login)
}

fn check(value: &Value) -> Check {
    let field = |key: &str| {
        display(
            value.get(key).and_then(Value::as_str).unwrap_or("unknown"),
            180,
        )
    };
    let kind = field("__typename");
    match kind.as_str() {
        "CheckRun" => Check {
            kind,
            name: field("name"),
            status: field("status"),
            conclusion: field("conclusion"),
        },
        "StatusContext" => Check {
            kind,
            name: field("context"),
            status: field("state"),
            conclusion: "not_applicable".into(),
        },
        _ => Check {
            kind: "unknown".into(),
            name: "Unrecognized check metadata".into(),
            status: "unknown".into(),
            conclusion: "unknown".into(),
        },
    }
}

pub fn inspect(repository: &str, number: u32, expected_account: &str) -> Result<Report, String> {
    inspect_with(repository, number, expected_account, &Gh)
}

fn inspect_with(
    repository: &str,
    number: u32,
    expected_account: &str,
    reader: &dyn Reader,
) -> Result<Report, String> {
    if !repository_ok(repository) || number == 0 || !login_ok(expected_account) {
        return Err("Enter a GitHub owner/repository, nonzero PR number and exact login".into());
    }
    let observed_account = account(reader, expected_account)?;
    let target = format!("github.com/{repository}");
    let number_arg = number.to_string();
    let bytes = reader.run(&[
        "pr",
        "view",
        &number_arg,
        "--repo",
        &target,
        "--json",
        FIELDS,
    ])?;
    let pr: Pull = serde_json::from_slice(&bytes)
        .map_err(|_| "GitHub returned incomplete or invalid PR metadata")?;
    let expected_url = format!("https://github.com/{repository}/pull/{number}");
    if pr.number != number
        || !pr.url.eq_ignore_ascii_case(&expected_url)
        || pr.head_ref_oid.len() != 40
        || !crate::worktrees::valid_commit(&pr.head_ref_oid)
        || !matches!(pr.state.as_str(), "OPEN" | "CLOSED" | "MERGED")
        || !matches!(
            pr.mergeable.as_str(),
            "MERGEABLE" | "CONFLICTING" | "UNKNOWN"
        )
    {
        return Err("GitHub returned a changed or unsupported PR identity".into());
    }
    // Detect an account switch while this read was in flight. No mutation
    // authority follows even when the account and all checks appear healthy.
    if account(reader, expected_account)? != observed_account {
        return Err("GitHub account changed during inspection".into());
    }
    let reported_checks = pr.status_check_rollup.len();
    let file_details_complete =
        pr.files.len() == pr.changed_files as usize && pr.files.len() <= MAX_FILES;
    Ok(Report {
        account: observed_account,
        repository: repository.into(),
        number,
        url: expected_url,
        title: display(&pr.title, 300),
        state: pr.state,
        draft: pr.is_draft,
        head_commit: pr.head_ref_oid,
        base: display(&pr.base_ref_name, 200),
        mergeability: pr.mergeable,
        review_decision: display(&pr.review_decision, 80),
        checks: pr
            .status_check_rollup
            .iter()
            .take(MAX_CHECKS)
            .map(check)
            .collect(),
        reported_checks,
        checks_truncated: reported_checks > MAX_CHECKS,
        files: pr
            .files
            .into_iter()
            .take(MAX_FILES)
            .map(|mut file| {
                file.path = display(&file.path, 400);
                file
            })
            .collect(),
        changed_files: pr.changed_files,
        file_details_complete,
        additions: pr.additions,
        deletions: pr.deletions,
        observed_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |time| time.as_secs()),
        read_only: true,
        note: "Observed CLI metadata only; checks may be incomplete or stale. This is not proof of code correctness, merge readiness, or permission to write.",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    struct Fake {
        responses: RefCell<VecDeque<Value>>,
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl Reader for Fake {
        fn run(&self, args: &[&str]) -> Result<Vec<u8>, String> {
            self.calls
                .borrow_mut()
                .push(args.iter().map(|arg| (*arg).into()).collect());
            let response = self
                .responses
                .borrow_mut()
                .pop_front()
                .ok_or("Unexpected request")?;
            serde_json::to_vec(&response).map_err(|error| error.to_string())
        }
    }

    fn fake(responses: Vec<Value>) -> Fake {
        Fake {
            responses: RefCell::new(responses.into()),
            calls: RefCell::new(vec![]),
        }
    }

    fn pr() -> Value {
        json!({
            "number": 7, "url": "https://github.com/example/repo/pull/7",
            "title": "Review fixture", "state": "OPEN", "isDraft": false,
            "headRefOid": "a".repeat(40), "baseRefName": "main",
            "mergeable": "UNKNOWN", "reviewDecision": "REVIEW_REQUIRED",
            "statusCheckRollup": [
                {"__typename":"CheckRun", "name":"Linux", "status":"COMPLETED", "conclusion":"SUCCESS"},
                {"__typename":"StatusContext", "context":"legacy", "state":"PENDING"}
            ],
            "changedFiles": 2, "files": [{"path":"src/main.rs", "additions":2, "deletions":1}],
            "additions": 2, "deletions": 1
        })
    }

    #[test]
    fn exact_read_uses_fixed_commands_and_distinguishes_partial_metadata() {
        let reader = fake(vec![
            json!({"login":"owner"}),
            pr(),
            json!({"login":"owner"}),
        ]);
        let result = inspect_with("example/repo", 7, "owner", &reader).unwrap();
        assert_eq!(result.head_commit, "a".repeat(40));
        assert_eq!(result.checks.len(), 2);
        assert_eq!(result.checks[0].conclusion, "SUCCESS");
        assert_eq!(result.checks[1].status, "PENDING");
        assert!(!result.file_details_complete);
        assert_eq!(result.mergeability, "UNKNOWN");
        assert!(result.read_only);
        let calls = reader.calls.borrow();
        assert_eq!(
            calls[0],
            ["api", "--hostname", "github.com", "--method", "GET", "user"]
        );
        assert_eq!(
            calls[1][..5],
            ["pr", "view", "7", "--repo", "github.com/example/repo"]
        );
        assert_eq!(calls[2], calls[0]);
    }

    #[test]
    fn rejects_wrong_account_and_repository_before_reading_pr() {
        let reader = fake(vec![json!({"login":"other"})]);
        assert!(inspect_with("example/repo", 7, "owner", &reader).is_err());
        assert_eq!(reader.calls.borrow().len(), 1);
        let reader = fake(vec![]);
        for repo in [
            "-x/repo",
            "https://github.com/example/repo",
            "owner/../repo",
            "owner/repo?x=1",
        ] {
            assert!(inspect_with(repo, 7, "owner", &reader).is_err());
        }
        assert!(reader.calls.borrow().is_empty());
    }

    #[test]
    fn rejects_stale_account_or_wrong_pr_identity() {
        let reader = fake(vec![
            json!({"login":"owner"}),
            pr(),
            json!({"login":"other"}),
        ]);
        assert!(inspect_with("example/repo", 7, "owner", &reader).is_err());
        let mut wrong = pr();
        wrong["url"] = json!("https://github.com/other/repo/pull/7");
        let reader = fake(vec![json!({"login":"owner"}), wrong]);
        assert!(inspect_with("example/repo", 7, "owner", &reader).is_err());
    }
}
