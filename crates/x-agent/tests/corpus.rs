//! A deterministic planning guard: ordinary dictation must never silently
//! become a desktop action. This corpus contains no private transcripts.
use x_agent::tools;

#[test]
fn corpus_distinguishes_actions_from_dictation_without_model_calls() {
    let mut actions = 0usize;
    let mut negatives = 0usize;
    for (i, line) in include_str!("fixtures/commands.tsv").lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (heard, expected) = line
            .split_once('\t')
            .unwrap_or_else(|| panic!("line {}: tab-separated classification required", i + 1));
        let plan = tools::plan_rules(heard);
        match expected {
            "NONE" => {
                negatives += 1;
                assert!(plan.is_none(), "line {}: dictation was claimed as a command: {heard:?}", i + 1);
            }
            "ACTION" => {
                actions += 1;
                assert!(plan.is_some(), "line {}: an expected command was missed: {heard:?}", i + 1);
                assert!(!plan.unwrap().is_empty(), "line {}: empty plan", i + 1);
            }
            _ => panic!("line {}: unknown corpus classification", i + 1),
        }
    }
    assert!(negatives >= 200, "must exercise at least 200 non-command sentences");
    assert!(actions >= 10, "must retain an explicit supported-command subset");
}
