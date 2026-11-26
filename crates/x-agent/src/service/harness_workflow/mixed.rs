//! Route typed graph nodes to the existing coding executor or desktop step host.
//! Both domains share the scheduler, reviewed task, Stop scopes and claim journal.

use super::*;
use crate::grants::Permission;
use crate::workflows::dag::Node;
use crate::workflows::desktop_exec::{AcquireInput, DesktopExecutor};
use crate::workflows::scheduler::Executor;

pub(super) struct GraphExecutor {
    coding: ApprovedHarnessExecutor,
    service: Service,
    task_id: u64,
    registry: Arc<Registry>,
    projects: HashMap<String, Project>,
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
    fn release_idle_projects(&self, active: &[&Node]) {
        let roots = active
            .iter()
            .filter_map(|node| self.projects.get(node.job.project_id()?))
            .map(|project| project.root.as_path())
            .collect::<Vec<_>>();
        self.service
            .release_idle_graph_projects(self.task_id, &roots);
    }
    fn try_acquire_project(&self, node: &Node) -> Result<bool, String> {
        let Some(id) = node.job.project_id() else {
            return Ok(true);
        };
        let expected = self
            .projects
            .get(id)
            .ok_or("The reviewed project is unavailable")?;
        let Some(reacquired) = self
            .service
            .try_reacquire_graph_project(self.task_id, &expected.root)
        else {
            return Ok(false);
        };
        if reacquired {
            let unchanged = self.registry.validated_resolve(id).is_ok_and(|current| {
                current.id == expected.id
                    && current.root == expected.root
                    && current.git_common_dir == expected.git_common_dir
            });
            if !unchanged {
                self.service
                    .release_graph_project(self.task_id, &expected.root);
                return Err("The registered project changed while the node waited".into());
            }
        }
        Ok(true)
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
            for (index, (node, job)) in workflow.graph.nodes.iter().zip(&workflow.jobs).enumerate()
            {
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
            let failure_service = self.clone();
            let correction = workflow.correction.clone();
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
            }))
            .with_failure(Arc::new(move |draft| {
                failure_service.remember_correction(draft, correction.as_ref());
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
            let projects = workflow
                .jobs
                .iter()
                .flatten()
                .map(|job| (job.project.id.to_string(), job.project.clone()))
                .collect();
            Ok(GraphExecutor {
                coding,
                service: self.clone(),
                task_id,
                registry: Arc::clone(&workflow.registry),
                projects,
                desktop,
            })
        })();
        if result.is_err() {
            for receipt in issued {
                let _ = grants.revoke(&receipt);
            }
        }
        result
    }
}
