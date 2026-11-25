use super::*;
use crate::service::edit::{step, task};
use crate::tools::EffectEvidence;

fn result(outcomes: Vec<Option<ExecutionOutcome>>) -> Task {
    let mut task = task(7, "fixture", vec![], None);
    task.phase = Phase::Completed;
    task.steps = outcomes
        .into_iter()
        .map(|execution| {
            let mut step = step("fixture", "local");
            step.status = "done".into(); // legacy text never supplies proof
            step.execution = execution;
            step
        })
        .collect();
    task
}

#[test]
fn aggregates_native_proof_without_promoting_launches_or_observations() {
    let confirmed = ExecutionOutcome::confirmed("matched", EffectEvidence::NativeWindowReadback);
    let observed = ExecutionOutcome::observed("label", EffectEvidence::AccessibilityObservation);
    let launched = ExecutionOutcome::launched("started");
    assert_eq!(
        result(vec![Some(confirmed.clone())]).action_outcome(),
        Some(ActionCompletion::Confirmed)
    );
    assert_eq!(
        result(vec![Some(observed.clone())]).action_outcome(),
        Some(ActionCompletion::Observed)
    );
    assert_eq!(
        result(vec![Some(launched.clone())]).action_outcome(),
        Some(ActionCompletion::Launched)
    );
    assert_eq!(
        result(vec![Some(confirmed.clone()), Some(launched)]).action_outcome(),
        Some(ActionCompletion::Partial)
    );
    assert_eq!(
        result(vec![Some(confirmed), Some(observed.clone())]).action_outcome(),
        Some(ActionCompletion::Partial)
    );
    assert_eq!(
        result(vec![Some(observed), None]).action_outcome(),
        Some(ActionCompletion::Unverified)
    );
    assert_eq!(result(vec![None]).action_outcome(), None);
}

#[test]
fn cancellation_and_failure_override_any_earlier_confirmation() {
    let mut task = result(vec![Some(ExecutionOutcome::confirmed(
        "matched",
        EffectEvidence::NativeWindowReadback,
    ))]);
    task.phase = Phase::Running;
    assert_eq!(task.action_outcome(), None);
    task.phase = Phase::Cancelled;
    assert_eq!(task.action_outcome(), Some(ActionCompletion::Cancelled));
    task.phase = Phase::Failed;
    assert_eq!(task.action_outcome(), Some(ActionCompletion::Failed));
}

#[test]
fn existing_step_json_fields_remain_parseable_with_additive_evidence() {
    let task = result(vec![Some(ExecutionOutcome::launched("process accepted"))]);
    let json = serde_json::to_value(&task.steps[0]).unwrap();
    #[derive(serde::Deserialize)]
    struct LegacyStep {
        title: String,
        status: String,
        risk: String,
    }
    let legacy: LegacyStep = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(legacy.title, "fixture");
    assert_eq!(legacy.status, "done");
    assert_eq!(legacy.risk, "local");
    assert_eq!(json["execution"]["state"], "launched");
    assert!(json["execution"].get("evidence").is_none());
}
