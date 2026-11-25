//! Inventory comes from the actual saved graph files, not a second task database.

use std::fs::File;

use serde::Serialize;
use uuid::Uuid;

use super::Store;
use crate::workflows::scheduler::Outcome;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct InventoryTask {
    pub graph_id: String,
    pub roadmap_id: String,
    pub task_id: String,
    pub goal_sha256: String,
    pub project_id: Uuid,
    /// Typed run outcome, not acceptance of the feature described in Markdown.
    pub last_outcome: Option<Outcome>,
}

impl InventoryTask {
    pub(crate) fn matches(
        &self,
        project: Uuid,
        roadmap: &str,
        task: &str,
        goal_hash: &str,
    ) -> bool {
        self.project_id == project
            && ((self.roadmap_id == roadmap && self.task_id == task)
                || self.goal_sha256 == goal_hash)
    }
}

impl Store {
    pub(crate) fn lock_roadmap_inventory(&self) -> Result<File, String> {
        self.ensure_private_dir()?;
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x0020_0000);
        }
        let file = options
            .open(self.dir.join("roadmap-inventory.lock"))
            .map_err(|_| "Cannot open roadmap inventory lock")?;
        if !file
            .metadata()
            .map_err(|_| "Cannot inspect roadmap inventory lock")?
            .is_file()
        {
            return Err("Roadmap inventory lock is not a regular file".into());
        }
        file.try_lock()
            .map_err(|_| "Another roadmap proposal is being saved; inspect again")?;
        Ok(file)
    }

    /// Failure is visible: an unreadable or oversized inventory must not silently
    /// make an existing task look new. No goals, provider output or source text.
    pub(crate) fn roadmap_inventory(&self) -> Result<Vec<InventoryTask>, String> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(_) => return Err("Cannot read saved workflow inventory".into()),
        };
        let mut files = Vec::new();
        for entry in entries {
            let path = entry
                .map_err(|_| "Cannot inspect saved workflow inventory")?
                .path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
                && !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().ends_with(".last.json"))
            {
                files.push(path);
                if files.len() > super::MAX_WORKFLOWS {
                    return Err("Saved workflow inventory exceeds 100 files; inspect it before importing more tasks".into());
                }
            }
        }
        files.sort();
        let mut tasks = Vec::new();
        for path in files {
            let text = Self::read(&path)
                .map_err(|_| "A saved workflow cannot be safely inspected for duplicate tasks")?;
            let graph = crate::workflows::dag::decode(&text, &self.home).map_err(
                |_| "A saved workflow is invalid; repair or remove it before importing tasks",
            )?;
            if path.file_stem().and_then(|name| name.to_str()) != Some(graph.id.as_str()) {
                return Err("A saved workflow has a mismatched file identity".into());
            }
            let Some(source) = graph.roadmap else {
                continue;
            };
            let last = self.last_graph(&graph.id)?;
            for task in source.tasks {
                let last_outcome = last
                    .as_ref()
                    .and_then(|last| last.nodes.iter().find(|node| node.id == task.node_id))
                    .map(|node| node.outcome);
                tasks.push(InventoryTask {
                    graph_id: graph.id.clone(),
                    roadmap_id: source.roadmap_id.clone(),
                    task_id: task.task_id,
                    goal_sha256: task.goal_sha256,
                    project_id: source.project_id,
                    last_outcome,
                });
            }
        }
        Ok(tasks)
    }
}
