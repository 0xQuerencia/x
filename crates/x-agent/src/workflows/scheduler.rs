//! Resource-limited DAG coordination. No desktop or harness is invoked by default.
//! Only an executor that explicitly authorizes every node can run a plan.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

use serde::{Deserialize, Serialize};

use super::dag::{Graph, Job, Node};
use crate::cancellation::Cancellation;

pub const MAX_PARALLEL: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Verified,
    Failed,
    Unverified,
    Cancelled,
    Skipped,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodeReport {
    pub id: String,
    pub outcome: Outcome,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub nodes: Vec<NodeReport>,
    pub stopped: bool,
    pub max_concurrent: usize,
}

/// A future harness/desktop adapter must gate *every* node against explicit
/// approval before the scheduler can run. There is intentionally no default.
pub trait Executor: Send + Sync + 'static {
    fn authorized(&self, node: &Node) -> bool;
    fn execute(&self, node: Node, stop: Cancellation) -> Outcome;
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum Resource {
    Desktop,
    Project(String),
}

fn resource(node: &Node) -> Resource {
    match &node.job {
        Job::Desktop { .. } => Resource::Desktop,
        Job::Harness { project_id, .. } => Resource::Project(project_id.clone()),
    }
}

/// Execute a previously user-authorized graph. The runner may only report
/// `Verified` after collecting independent evidence, never from model text.
/// One writable job per project is allowed until worktree isolation lands.
pub fn run(
    graph: &Graph,
    home: &Path,
    executor: Arc<dyn Executor>,
    stop: &Cancellation,
    parallel: usize,
) -> Result<Report, String> {
    graph.validate_with_home(home)?;
    if !(1..=MAX_PARALLEL).contains(&parallel) {
        return Err(format!("Concurrent workers must be 1-{MAX_PARALLEL}"));
    }
    // A permission failure blocks the *entire* graph before its first side effect.
    if graph.nodes.iter().any(|node| !executor.authorized(node)) {
        return Err("A workflow node needs explicit approval".into());
    }

    let ids: HashMap<&str, usize> = graph
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id.as_str(), index))
        .collect();
    let count = graph.nodes.len();
    let mut finished: Vec<Option<Outcome>> = vec![None; count];
    let mut active: HashSet<usize> = HashSet::new();
    let mut resources: HashSet<Resource> = HashSet::new();
    let mut notes = vec![String::new(); count];
    let (send, receive) = mpsc::channel::<(usize, Outcome)>();
    let mut stop_new = false;
    let mut max_concurrent = 0usize;

    loop {
        let mut progressed = false;
        if stop.requested() {
            stop_new = true;
        }
        if !stop_new {
            for (index, node) in graph.nodes.iter().enumerate() {
                if finished[index].is_some() || active.contains(&index) {
                    continue;
                }
                let all_finished = node
                    .after
                    .iter()
                    .chain(&node.requires)
                    .all(|dependency| finished[ids[dependency.as_str()]].is_some());
                if !all_finished {
                    continue;
                }
                let required_ok = node
                    .requires
                    .iter()
                    .all(|dependency| finished[ids[dependency.as_str()]] == Some(Outcome::Verified));
                if !required_ok {
                    finished[index] = Some(Outcome::Skipped);
                    notes[index] = "Required predecessor was not verified".into();
                    progressed = true;
                    continue;
                }
                if active.len() >= parallel {
                    break;
                }
                let owned_resource = resource(node);
                if resources.contains(&owned_resource) {
                    continue;
                }
                // Revoked permissions must also be checked right before dispatch.
                if !executor.authorized(node) {
                    finished[index] = Some(Outcome::Failed);
                    notes[index] = "Approval was revoked before dispatch".into();
                    if node.on_failure == super::model::OnFailure::Stop {
                        stop_new = true;
                    }
                    progressed = true;
                    continue;
                }
                resources.insert(owned_resource);
                active.insert(index);
                max_concurrent = max_concurrent.max(active.len());
                progressed = true;
                let runner = Arc::clone(&executor);
                let tx = send.clone();
                let task = node.clone();
                let cancellation = stop.child();
                thread::spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        runner.execute(task, cancellation)
                    }))
                    .unwrap_or(Outcome::Failed);
                    let _ = tx.send((index, result));
                });
            }
        }

        if active.is_empty() {
            if !progressed || stop_new || finished.iter().all(Option::is_some) {
                break;
            }
            continue;
        }
        let Ok((index, outcome)) = receive.recv() else {
            return Err("The scheduler lost an owned worker".into());
        };
        active.remove(&index);
        resources.remove(&resource(&graph.nodes[index]));
        finished[index] = Some(outcome);
        if matches!(outcome, Outcome::Failed | Outcome::Unverified)
            && graph.nodes[index].on_failure == super::model::OnFailure::Stop
        {
            stop_new = true;
        }
        if outcome == Outcome::Cancelled {
            stop_new = true;
        }
    }

    let interrupted = stop.requested();
    let nodes = graph
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| NodeReport {
            id: node.id.clone(),
            outcome: finished[index].unwrap_or(if interrupted {
                Outcome::Cancelled
            } else {
                Outcome::Skipped
            }),
            reason: if !notes[index].is_empty() {
                notes[index].clone()
            } else if finished[index].is_none() && interrupted {
                "Global Stop prevented dispatch".into()
            } else if finished[index].is_none() {
                "Earlier task stopped the workflow".into()
            } else {
                String::new()
            },
        })
        .collect();
    Ok(Report {
        nodes,
        stopped: interrupted,
        max_concurrent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::dag::{HarnessKind, Node};
    use super::super::model::OnFailure;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    struct Mock {
        authorized: bool,
        running: AtomicUsize,
        peak: AtomicUsize,
        calls: AtomicUsize,
        outcome: Outcome,
    }

    impl Mock {
        fn new(outcome: Outcome) -> Self {
            Self {
                authorized: true,
                running: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
                calls: AtomicUsize::new(0),
                outcome,
            }
        }
    }

    impl Executor for Mock {
        fn authorized(&self, _node: &Node) -> bool {
            self.authorized
        }

        fn execute(&self, _node: Node, cancel: Cancellation) -> Outcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let current = self.running.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(current, Ordering::SeqCst);
            for _ in 0..20 {
                if cancel.requested() {
                    self.running.fetch_sub(1, Ordering::SeqCst);
                    return Outcome::Cancelled;
                }
                thread::sleep(Duration::from_millis(2));
            }
            self.running.fetch_sub(1, Ordering::SeqCst);
            self.outcome
        }
    }

    fn job(id: &str, project_id: &str, requires: &[&str]) -> Node {
        Node {
            id: id.into(),
            after: vec![],
            requires: requires.iter().map(|text| (*text).into()).collect(),
            job: Job::Harness {
                harness: HarnessKind::OpenCode,
                project_id: project_id.into(),
                goal: "review tests".into(),
            },
            on_failure: OnFailure::Continue,
        }
    }

    fn graph(first: &str, second: &str) -> Graph {
        Graph {
            version: super::super::dag::VERSION,
            id: "parallel-fixture".into(),
            name: "Parallel fixture".into(),
            triggers: vec![],
            nodes: vec![job("front", first, &[]), job("back", second, &[])],
        }
    }

    #[test]
    fn independent_projects_can_run_concurrently() {
        let a = uuid::Uuid::new_v4().to_string();
        let b = uuid::Uuid::new_v4().to_string();
        let graph = graph(&a, &b);
        let mock = Arc::new(Mock::new(Outcome::Verified));
        let report = run(&graph, Path::new("/tmp"), mock.clone(), &Cancellation::default(), 2)
            .unwrap();
        assert_eq!(report.max_concurrent, 2);
        assert_eq!(mock.peak.load(Ordering::SeqCst), 2);
        assert!(report.nodes.iter().all(|node| node.outcome == Outcome::Verified));
    }

    #[test]
    fn writing_jobs_cannot_share_the_same_project_concurrently() {
        let id = uuid::Uuid::new_v4().to_string();
        let graph = graph(&id, &id);
        let mock = Arc::new(Mock::new(Outcome::Verified));
        let report = run(&graph, Path::new("/tmp"), mock.clone(), &Cancellation::default(), 2)
            .unwrap();
        assert_eq!(report.max_concurrent, 1);
        assert_eq!(mock.peak.load(Ordering::SeqCst), 1);
        assert_eq!(mock.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_failed_required_dependency_never_releases_its_child() {
        let a = uuid::Uuid::new_v4().to_string();
        let b = uuid::Uuid::new_v4().to_string();
        let mut graph = graph(&a, &b);
        graph.nodes[1].requires = vec!["front".into()];
        let mock = Arc::new(Mock::new(Outcome::Unverified));
        let report = run(&graph, Path::new("/tmp"), mock.clone(), &Cancellation::default(), 2)
            .unwrap();
        assert_eq!(report.nodes[0].outcome, Outcome::Unverified);
        assert_eq!(report.nodes[1].outcome, Outcome::Skipped);
        assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn no_work_starts_without_preflight_approval() {
        struct Deny;
        impl Executor for Deny {
            fn authorized(&self, _node: &Node) -> bool { false }
            fn execute(&self, _node: Node, _stop: Cancellation) -> Outcome {
                panic!("unapproved work was executed");
            }
        }
        let a = uuid::Uuid::new_v4().to_string();
        let b = uuid::Uuid::new_v4().to_string();
        let result = run(&graph(&a, &b), Path::new("/tmp"), Arc::new(Deny), &Cancellation::default(), 2);
        assert!(result.is_err());
    }

    #[test]
    fn global_stop_reaches_running_workers_and_skips_unstarted_work() {
        let a = uuid::Uuid::new_v4().to_string();
        let b = uuid::Uuid::new_v4().to_string();
        let graph = graph(&a, &b);
        let root = Cancellation::default();
        let cancellation = root.clone();
        let mock = Arc::new(Mock::new(Outcome::Verified));
        let trigger = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            cancellation.request();
        });
        let report = run(&graph, Path::new("/tmp"), mock, &root, 1).unwrap();
        trigger.join().unwrap();
        assert!(report.stopped);
        assert_eq!(report.nodes[0].outcome, Outcome::Cancelled);
        assert_eq!(report.nodes[1].outcome, Outcome::Cancelled);
    }
}
