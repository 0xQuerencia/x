//! Route typed graph nodes to the existing coding executor or desktop step host.
//! Both domains share the scheduler, reviewed task, Stop scopes and claim journal.

use super::*;
use crate::grants::Permission;
use crate::workflows::dag::Node;
use crate::workflows::desktop_exec::{AcquireInput, DesktopExecutor};
use crate::workflows::scheduler::Executor;

pub(super) struct GraphExecutor {
    coding: ApprovedHarnessExecutor,
    pub(super) desktop: DesktopExecutor,
}

impl Executor for GraphExecutor {
    fn authorized(&self, node: &Node) -> bool {
        match node.job {
            Job::Desktop { .. } => self.desktop.authorized(node),
            _ => self.coding.authorized(node),
        }
    }
    fn verified_worktree_key(&self, node: &Node) -> Option<String> {
        self.coding.verified_worktree_key(node)
    }
    fn outcome_reason(&self, node: &Node) -> Option<String> {
        match node.job {
            Job::Desktop { .. } => self.desktop.outcome_reason(node),
            _ => self.coding.outcome_reason(node),
        }
    }
    fn execute(&self, node: Node, stop: Cancellation) -> Outcome {
        match node.job {
            Job::Desktop { .. } => self.desktop.execute(node, stop),
            _ => self.coding.execute(node, stop),
        }
    }
}

impl Service {
    pub(super) fn approved_graph_executor(
        &self,
        workflow: &PendingGraph,
        run: Uuid,
        stop: &Cancellation,
        stops: &JobStops,
    ) -> Result<GraphExecutor, String> {
        let grants = self.scoped_grants();
        let mut issued = Vec::new();
        let result = (|| {
            let state = self.lock();
            stop.check()?;
            let task_id = state
                .task
                .as_ref()
                .ok_or("The graph no longer has an approved task")?
                .id;
            let mut assignments = Vec::new();
            for (index, (node, job)) in workflow.graph.nodes.iter().zip(&workflow.jobs).enumerate() {
                let Some(job) = job else {
                    continue;
                };
                let proof = proof_for(node);
                let scope = planned_scope(node, job.project.id, run, &job.commit, &proof)?;
                let receipt = grants.issue(
                    queue_permission(node),
                    scope.clone(),
                    Duration::from_secs(30 * 60),
                    1,
                    true,
                )?;
                issued.push(receipt);
                stops.register_renewal(
                    index,
                    Arc::clone(&grants),
                    receipt,
                    queue_permission(node),
                    scope,
                )?;
                assignments.push(Assignment::from_planned_scope(
                    node,
                    job.project.clone(),
                    run,
                    job.commit.clone(),
                    job.adapter.clone(),
                    proof,
                    Arc::clone(&grants),
                    receipt,
                )?);
            }
            let mut desktop = Vec::new();
            for prepared in &workflow.desktop {
                let receipt = grants.issue(
                    Permission::QueueDesktop,
                    prepared.scope(run, None)?,
                    Duration::from_secs(30 * 60),
                    1,
                    true,
                )?;
                issued.push(receipt);
                desktop.push((prepared.clone(), receipt));
            }
            drop(state);
            stop.check()?;
            let service = self.clone();
            let coding = ApprovedHarnessExecutor::new(
                &workflow.graph,
                run,
                assignments,
                Arc::clone(&workflow.manager),
                Arc::clone(&workflow.journal),
            )?
            .with_activity(Arc::new(move |node, attempt, kind| {
                service.emit(
                    "workflow_activity",
                    &format!("{run} / {node} / attempt {attempt}: {}", kind.as_str()),
                );
            }));
            let service = self.clone();
            let acquire: AcquireInput = Arc::new(move |stop| {
                Ok(Box::new(service.acquire_desktop(
                    task_id,
                    stop,
                    Duration::from_secs(15),
                )?))
            });
            let desktop = DesktopExecutor::new(
                run,
                desktop,
                self.machine(),
                crate::workflows::Config {
                    home: workflow.home.clone(),
                    poll: Duration::from_millis(150),
                },
                acquire,
                Arc::clone(&grants),
                Arc::clone(&workflow.journal),
            )?;
            Ok(GraphExecutor { coding, desktop })
        })();
        if result.is_err() {
            for receipt in issued {
                let _ = grants.revoke(&receipt);
            }
        }
        result
    }
}
