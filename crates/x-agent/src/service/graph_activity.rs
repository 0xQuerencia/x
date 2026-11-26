//! A bounded native view of one graph's actual schedule and dispatch progress.

use std::sync::Arc;
use std::time::Instant;

use uuid::Uuid;

use crate::workflows::dag::{Graph, Job};
use crate::workflows::harness_exec::CodingProgress;
use crate::workflows::scheduler::{NodeUpdate, Outcome};

use super::{Service, State};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphJobPhase {
    Queued,
    Preparing,
    Recovering,
    Running,
    Waiting,
    Verified,
    Tested,
    Delivered,
    Launched,
    Failed,
    Unverified,
    Stopped,
    Skipped,
}

impl GraphJobPhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Preparing => "Preparing checkout",
            Self::Recovering => "Reattaching recorded checkout",
            Self::Running => "Running",
            Self::Waiting => "Waiting for approval",
            Self::Verified => "Verified",
            Self::Tested => "Tested",
            Self::Delivered => "Delivery recorded",
            Self::Launched => "Launched",
            Self::Failed => "Failed",
            Self::Unverified => "Unverified",
            Self::Stopped => "Stopped",
            Self::Skipped => "Skipped",
        }
    }

    fn finished(outcome: Outcome) -> Self {
        match outcome {
            Outcome::Verified => Self::Verified,
            Outcome::Tested => Self::Tested,
            Outcome::Delivered => Self::Delivered,
            Outcome::Launched => Self::Launched,
            Outcome::Failed => Self::Failed,
            Outcome::Unverified => Self::Unverified,
            Outcome::Cancelled => Self::Stopped,
            Outcome::Skipped => Self::Skipped,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphJobView {
    pub run: Uuid,
    pub index: usize,
    pub node: Arc<str>,
    pub dependencies: Arc<[String]>,
    pub phase: GraphJobPhase,
    pub elapsed_seconds: u64,
    pub has_checkout: bool,
}

struct Entry {
    node: Arc<str>,
    dependencies: Arc<[String]>,
    phase: GraphJobPhase,
    started: Option<Instant>,
    ended: Option<Instant>,
    coding: bool,
    checkout_recorded: bool,
}

pub(super) struct GraphActivity {
    task: u64,
    run: Uuid,
    queued: Instant,
    recovery: bool,
    jobs: Vec<Entry>,
}

impl GraphActivity {
    fn update(&mut self, index: usize, update: NodeUpdate, now: Instant) {
        let Some(job) = self.jobs.get_mut(index) else {
            return;
        };
        match update {
            NodeUpdate::WaitingForPermission => job.phase = GraphJobPhase::Waiting,
            NodeUpdate::Running => {
                job.started.get_or_insert(now);
                job.phase = match (job.coding, self.recovery) {
                    (true, true) => GraphJobPhase::Recovering,
                    (true, false) => GraphJobPhase::Preparing,
                    (false, _) => GraphJobPhase::Running,
                };
            }
            NodeUpdate::Finished(outcome) => {
                job.ended = Some(now);
                job.phase = GraphJobPhase::finished(outcome);
            }
        }
    }

    fn snapshot(&self, now: Instant) -> Vec<GraphJobView> {
        self.jobs
            .iter()
            .enumerate()
            .map(|(index, job)| GraphJobView {
                run: self.run,
                index,
                node: Arc::clone(&job.node),
                dependencies: Arc::clone(&job.dependencies),
                phase: job.phase,
                elapsed_seconds: job
                    .ended
                    .unwrap_or(now)
                    .saturating_duration_since(job.started.unwrap_or(self.queued))
                    .as_secs(),
                has_checkout: job.ended.is_some() && job.checkout_recorded,
            })
            .collect()
    }
}

impl State {
    pub(super) fn graph_jobs(&self) -> Vec<GraphJobView> {
        match (&self.graph_activity, &self.task) {
            (Some(activity), Some(task)) if activity.task == task.id => {
                activity.snapshot(Instant::now())
            }
            _ => Vec::new(),
        }
    }
}

impl Service {
    pub(super) fn start_graph_activity(
        &self,
        run: Uuid,
        graph: &Graph,
        recovery: bool,
    ) -> Result<(), String> {
        let mut state = self.lock();
        let task = state
            .task
            .as_ref()
            .ok_or("The approved graph task disappeared")?
            .id;
        let jobs = graph
            .nodes
            .iter()
            .map(|node| {
                let mut dependencies = node
                    .after
                    .iter()
                    .chain(&node.requires)
                    .chain(&node.requires_verified)
                    .chain(node.inputs.iter().map(|input| &input.node))
                    .cloned()
                    .collect::<Vec<_>>();
                dependencies.sort();
                dependencies.dedup();
                Entry {
                    node: Arc::from(node.id.as_str()),
                    dependencies: Arc::from(dependencies),
                    phase: GraphJobPhase::Queued,
                    started: None,
                    ended: None,
                    coding: !matches!(node.job, Job::Desktop { .. }),
                    checkout_recorded: false,
                }
            })
            .collect();
        state.graph_activity = Some(GraphActivity {
            task,
            run,
            queued: Instant::now(),
            recovery,
            jobs,
        });
        Ok(())
    }

    pub(super) fn record_graph_update(&self, run: Uuid, index: usize, update: NodeUpdate) {
        let mut state = self.lock();
        if let Some(activity) = state
            .graph_activity
            .as_mut()
            .filter(|activity| activity.run == run)
        {
            activity.update(index, update, Instant::now());
        }
    }

    pub(super) fn record_graph_progress(
        &self,
        task: u64,
        run: Uuid,
        node: &str,
        progress: CodingProgress,
    ) {
        let mut state = self.lock();
        if let Some(job) = state
            .graph_activity
            .as_mut()
            .filter(|activity| activity.task == task && activity.run == run)
            .and_then(|activity| {
                activity
                    .jobs
                    .iter_mut()
                    .find(|job| job.node.as_ref() == node)
            })
            && job.ended.is_none()
        {
            match progress {
                CodingProgress::CheckoutRecorded => job.checkout_recorded = true,
                CodingProgress::Dispatch => job.phase = GraphJobPhase::Running,
            }
        }
    }
}
