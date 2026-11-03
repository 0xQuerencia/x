use super::*;

fn tree() -> Value {
    serde_json::json!({
        "role": "frame", "name": "Page", "children": [
            {"role": "button", "name": "Press me", "ref_id": "@s:e1", "available_actions": ["Click"], "children": [
                {"role": "statictext", "name": "Press me"}]},
            {"role": "textfield", "name": "Address and search bar", "value": "https://a.example", "ref_id": "@s:e2",
             "available_actions": ["SetFocus"]},
            {"role": "textfield", "name": "Password", "value": "hunter2", "ref_id": "@s:e3", "available_actions": ["SetFocus"]},
            {"role": "button", "name": "Hidden", "ref_id": "@s:e4", "available_actions": ["Click"], "states": ["hidden"]},
            {"role": "statictext", "name": "clicked"}
        ]
    })
}

fn observed() -> Observation {
    let mut obs = Observation {
        title: "T".into(),
        ..Observation::default()
    };
    collect(&tree(), "", &mut obs);
    obs
}

#[test]
fn collects_controls() {
    let obs = observed();
    let names: Vec<&str> = obs.elements.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["Press me", "Address and search bar", "Password"]);
    assert_eq!(obs.texts, ["clicked"]);
    assert_eq!(obs.element("e2").unwrap().reference, "@s:e2");
    assert!(obs.shows("CLICKED") && !obs.shows("hunter2") && !obs.shows("no"));
}

#[test]
fn hides_secrets() {
    let rendered = observed().render();
    assert!(rendered.contains("https://a.example"));
    assert!(!rendered.contains("hunter2"));
    assert!(observed().element("e3").unwrap().is_secret());
    assert!(observed().element("e2").unwrap().is_omnibox());
}

#[test]
fn prompt_text_redacts_email_phone_and_long_tokens() {
    let safe = prompt_text(
        "Contact alice@example.org or 555-123-4567 token abcdefghijklmnopqrstuvwxyz1234567890",
    );
    assert!(!safe.contains("alice@example.org"));
    assert!(!safe.contains("555-123-4567"));
    assert!(!safe.contains("abcdefghijklmnopqrstuvwxyz1234567890"));
}

#[test]
fn key_allowlist() {
    assert_eq!(Key::parse("enter"), Some(Key::Enter));
    assert!(Key::parse("ctrl+w").is_none() && Key::parse("super+q").is_none());
}
