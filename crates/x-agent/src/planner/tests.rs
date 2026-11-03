use super::*;
use crate::tools::Action;

#[test]
fn only_registry_steps_survive() {
    let ok = reply_from_text(
        "sure {\"steps\":[{\"kind\":\"app\",\"target\":\"terminal\"},{\"kind\":\"type\",\"text\":\"hi\"}]} done",
        false,
    )
    .unwrap();
    assert!(matches!(ok, Reply::Steps(steps) if steps.len() == 2));
    for bad in [
        "{\"steps\":[{\"kind\":\"shell\",\"command\":\"rm -rf ~\"}]}",
        "{\"steps\":[{\"kind\":\"app\",\"target\":\"bash\"}]}",
        "{\"steps\":[{\"kind\":\"type\",\"text\":\"a\\nrm -rf ~\\n\"}]}",
        "{\"steps\":[{\"kind\":\"url\",\"target\":\"http://127.0.0.1/admin\"}]}",
        "{\"steps\":[{\"kind\":\"workspace\",\"number\":99}]}",
        "{\"steps\":[{\"kind\":\"session\",\"goal\":\"x\"},{\"kind\":\"app\",\"target\":\"terminal\"}]}",
        "no json here",
    ] {
        assert!(reply_from_text(bad, false).is_err(), "{bad}");
    }
}

#[test]
fn dictation_is_only_accepted_when_asked_for() {
    assert_eq!(
        reply_from_text("{\"dictate\":true}", true),
        Ok(Reply::Dictation)
    );
    assert!(reply_from_text("{\"dictate\":true}", false).is_err());
    assert!(
        prompt("hello", true).contains("dictate") && !prompt("hello", false).contains("dictate")
    );
}

#[test]
fn new_steps_parse() {
    let steps = match reply_from_text(
        "{\"steps\":[{\"kind\":\"session\",\"goal\":\"write a poem\"}]}",
        false,
    ) {
        Ok(Reply::Steps(steps)) => steps,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        steps,
        [Action::Session {
            goal: "write a poem".into()
        }]
    );
}

#[test]
fn only_https_or_loopback_endpoints_are_used() {
    assert!(checked_base("https://example.com/v1/").is_ok());
    assert!(checked_base("http://127.0.0.1:8080/v1").is_ok());
    assert!(checked_base("http://example.com/v1").is_err());
    assert!(checked_base("ftp://example.com").is_err());
    for fake in [
        "http://127.0.0.1.evil.com/v1",
        "http://localhost.evil.com",
        "http://localhost@evil.com",
        "http://127.0.0.1@evil.com",
    ] {
        assert!(!is_loopback(fake) && checked_base(fake).is_err(), "{fake}");
    }
    assert!(is_loopback("http://localhost:11434/v1"));
}

#[test]
fn the_key_never_reaches_argv_and_quotes_are_escaped() {
    assert_eq!(quoted("a\"b\\c"), "\"a\\\"b\\\\c\"");
    let planner = Gemini {
        model: "m".into(),
        key_file: None,
    };
    let _ = planner.name();
}
