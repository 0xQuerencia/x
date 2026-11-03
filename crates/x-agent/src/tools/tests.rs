use super::*;

fn fake_path(name: &str) -> Option<String> {
    Some(format!("/usr/bin/{name}"))
}

fn plan(text: &str) -> Vec<String> {
    plan_rules(text)
        .unwrap_or_default()
        .iter()
        .map(Action::describe)
        .collect()
}

#[test]
fn typing_is_plain_text_only_and_follows_a_launch_to_be_low_risk() {
    let plan = plan_rules("open a terminal and type welcome back wiki").unwrap();
    assert_eq!(
        plan.iter().map(Action::describe).collect::<Vec<_>>(),
        [
            "Open terminal",
            "Type \"welcome back wiki\" into the focused window"
        ]
    );
    assert!(is_low_risk(&plan));
    assert!(!is_low_risk(&plan_rules("type hello").unwrap()));
    let argv = plan[1].argv(&fake_path).unwrap();
    assert_eq!(argv, ["/usr/bin/wtype", "--", "welcome back wiki"]);
    assert!(
        Action::Type {
            text: "a\nb".into()
        }
        .validate()
        .is_err()
    );
    assert!(
        Action::Type {
            text: "x\u{1b}[A".into()
        }
        .validate()
        .is_err()
    );
    assert!(Action::Type { text: "  ".into() }.validate().is_err());
    assert_eq!(plan_rules("open up a terminal").unwrap().len(), 1);
    let dash = Action::Type {
        text: "--help".into(),
    };
    assert_eq!(dash.argv(&fake_path).unwrap()[1], "--");
}

#[test]
fn an_opencode_session_request_opens_opencode_in_a_terminal_not_a_headless_run() {
    for said in [
        "Open code session. Start a open code session",
        "start an opencode session",
        "spin up an OpenCode session",
    ] {
        assert_eq!(plan_rules(said), Some(vec![Action::Opencode]), "{said}");
    }
    assert!(matches!(
        plan_rules("start a temporary session to write a poem").unwrap()[0],
        Action::Session { .. }
    ));
    let dir = std::path::Path::new("/home/u/x-sessions/s");
    let argv = opencode_argv(dir, &|n| Some(format!("/usr/bin/{n}"))).unwrap();
    assert_eq!(
        argv[..3],
        ["/usr/bin/kitty", "--directory", "/home/u/x-sessions/s"]
    );
    assert!(is_low_risk(&[Action::Opencode]));
}

#[test]
fn opens_known_apps_and_sites() {
    assert_eq!(plan("open Firefox"), ["Open firefox"]);
    assert_eq!(plan("Hey X, launch VS Code."), ["Open vscode"]);
    assert_eq!(plan("open github"), ["Open https://github.com"]);
    assert_eq!(plan("open example.org"), ["Open https://example.org"]);
}

#[test]
fn wake_word_variants() {
    for utterance in [
        "Friday, go to workspace 3",
        "Hey Friday, go to workspace 3",
        "Hey, Friday, go to workspace 3",
    ] {
        assert_eq!(strip_wake_word(utterance), "go to workspace 3");
    }
    assert_eq!(
        strip_wake_word("Friday, use Chrome to find the latest PR"),
        "use Chrome to find the latest PR"
    );
}

#[test]
fn custom_wake_words() {
    let mine = ["jarvis".to_string(), "computer".to_string()];
    assert_eq!(
        strip_with("Hey Jarvis, open Firefox", &mine),
        "open Firefox"
    );
    assert_eq!(strip_with("computer open firefox", &mine), "open firefox");
    assert_eq!(
        strip_with("Friday open firefox", &mine),
        "Friday open firefox"
    );
    assert!(valid_wake_word("Jarvis").is_ok() && valid_wake_word("hey").is_err());
    assert!(valid_wake_word("two words").is_err() && valid_wake_word("").is_err());
    assert!(valid_wake_word("work").is_err());
}

#[test]
fn browser_plus_search_is_one_launch() {
    let actions = plan_rules("open Firefox and search YouTube for Mars rover").unwrap();
    assert_eq!(actions.len(), 1);
    let argv = actions[0].argv(&fake_path).unwrap();
    assert_eq!(
        argv,
        [
            "/usr/bin/firefox",
            "https://www.youtube.com/results?search_query=Mars%20rover"
        ]
    );
}

#[test]
fn two_step_instruction() {
    assert_eq!(
        plan("open terminal then search github for ripgrep"),
        ["Open terminal", "Search github for \"ripgrep\""]
    );
    assert_eq!(
        plan("open chrome then search github for ripgrep"),
        ["Search github for \"ripgrep\" in chrome"]
    );
}

#[test]
fn and_inside_a_query_is_kept() {
    assert_eq!(
        plan("search google for salt and pepper"),
        ["Search google for \"salt and pepper\""]
    );
}

#[test]
fn query_cannot_inject_arguments_or_shell() {
    let a = plan_rules("search youtube for a&b; rm -rf ~ $(whoami) `id` --new-window").unwrap();
    let argv = a[0].argv(&fake_path).unwrap();
    assert_eq!(argv.len(), 2, "query stays inside one URL argument");
    assert!(!argv[1].contains(['&', ';', ' ', '$', '`']), "{}", argv[1]);
}

#[test]
fn unsupported_requests_run_nothing() {
    for t in [
        "delete all my files",
        "run rm -rf /",
        "send an email to bob",
        "",
        "open /etc/passwd",
    ] {
        assert!(plan_rules(t).is_none(), "{t}");
    }
    assert!(plan_rules(&"open firefox ".repeat(200)).is_none());
}

#[test]
fn non_ascii_input_never_panics() {
    for t in [
        "İstanbul open firefox",
        "open İ firefox and search ß for ǅ",
        "ǅ then open ǅ",
        "x\u{130}, open firefox",
        "search youtube for café and open github",
        "open 日本語.jp",
    ] {
        let _ = plan_rules(t);
    }
    assert_eq!(
        plan("search youtube for café"),
        ["Search youtube for \"café\""]
    );
}

#[test]
fn urls_are_locked_down() {
    for bad in [
        "http://example.com",
        "file:///etc/passwd",
        "https://0x7f.0.0.1/",
        "https://0xc0.0xa8.1.1/",
        "https://nas.lan/",
        "https://app.internal/",
        "javascript:alert(1)",
        "https://localhost/",
        "https://a.local/",
        "https://127.0.0.1/",
        "https://user:pw@example.com/",
        "https://example.com:8080/",
        "https://exa mple.com",
        "https://-bad.com",
        "https://nodots",
        "https://example.com/\u{0007}",
    ] {
        assert!(safe_url(bad).is_err(), "{bad}");
    }
    assert!(safe_url("https://example.com:443/a?b=c").is_ok());
}

#[test]
fn planner_json_is_untrusted() {
    let ok = r#"{"steps":[{"kind":"search","target":"youtube","query":"cats"}]}"#;
    assert_eq!(parse_plan_json(ok).unwrap().len(), 1);
    for bad in [
        r#"{"steps":[{"kind":"shell","cmd":"rm -rf /"}]}"#,
        r#"{"steps":[{"kind":"app","target":"bash"}]}"#,
        r#"{"steps":[{"kind":"app","target":"firefox","extra":"--evil"}]}"#,
        r#"{"steps":[{"kind":"url","target":"file:///etc/passwd"}]}"#,
        r#"{"steps":[{"kind":"url","target":"https://example.com","browser":"curl"}]}"#,
        r#"{"steps":[{"kind":"search","target":"evil","query":"x"}]}"#,
        r#"{"steps":[{"kind":"app","target":"firefox"},{"kind":"app","target":"firefox"},{"kind":"app","target":"firefox"},{"kind":"app","target":"firefox"},{"kind":"app","target":"firefox"},{"kind":"app","target":"firefox"}]}"#,
        "not json",
        r#"{"steps":"x"}"#,
    ] {
        assert!(parse_plan_json(bad).is_err(), "{bad}");
    }
}

#[test]
fn missing_executable_is_reported() {
    let a = Action::App {
        target: "firefox".into(),
    };
    assert!(a.argv(&|_| None).unwrap_err().contains("not installed"));
}

#[test]
fn computer_browsers_only() {
    for app in ["terminal", "vscode", "files"] {
        assert!(
            Action::Computer {
                app: app.into(),
                goal: "x".into()
            }
            .validate()
            .is_err(),
            "{app}"
        );
    }
    assert!(
        Action::Computer {
            app: "chrome".into(),
            goal: "x".into()
        }
        .validate()
        .is_ok()
    );
}
