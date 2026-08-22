use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TaskStatus {
    Pending,
    Ready,
    Running,
    Completed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskNode {
    pub id: String,
    pub kind: String,
    pub dependencies: Vec<String>,
    pub status: TaskStatus,
}

#[derive(Debug, Clone, Default)]
pub struct TaskGraph {
    pub nodes: HashMap<String, TaskNode>,
}

impl TaskGraph {
    pub fn add(&mut self, id: &str, kind: &str, dependencies: &[&str]) -> Result<()> {
        if self.nodes.contains_key(id) {
            return Err(anyhow!("duplicate task node {id}"));
        }
        self.nodes.insert(
            id.into(),
            TaskNode {
                id: id.into(),
                kind: kind.into(),
                dependencies: dependencies.iter().map(|s| (*s).into()).collect(),
                status: TaskStatus::Pending,
            },
        );
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        for node in self.nodes.values() {
            for dep in &node.dependencies {
                if !self.nodes.contains_key(dep) {
                    return Err(anyhow!("{} depends on missing node {dep}", node.id));
                }
            }
        }
        fn visit(
            id: &str,
            g: &TaskGraph,
            visiting: &mut HashSet<String>,
            done: &mut HashSet<String>,
        ) -> Result<()> {
            if done.contains(id) {
                return Ok(());
            }
            if !visiting.insert(id.into()) {
                return Err(anyhow!("task graph contains a cycle at {id}"));
            }
            for dep in &g.nodes[id].dependencies {
                visit(dep, g, visiting, done)?;
            }
            visiting.remove(id);
            done.insert(id.into());
            Ok(())
        }
        let mut visiting = HashSet::new();
        let mut done = HashSet::new();
        for id in self.nodes.keys() {
            visit(id, self, &mut visiting, &mut done)?
        }
        Ok(())
    }

    pub fn ready(&self) -> Vec<String> {
        self.nodes
            .values()
            .filter(|node| {
                node.status == TaskStatus::Pending
                    && node.dependencies.iter().all(|dep| {
                        self.nodes
                            .get(dep)
                            .is_some_and(|n| n.status == TaskStatus::Completed)
                    })
            })
            .map(|n| n.id.clone())
            .collect()
    }

    pub fn set_status(&mut self, id: &str, status: TaskStatus) -> Result<()> {
        self.nodes
            .get_mut(id)
            .ok_or_else(|| anyhow!("unknown task node {id}"))?
            .status = status;
        Ok(())
    }
}

pub fn default_workflow() -> TaskGraph {
    let mut g = TaskGraph::default();
    g.add("inspect", "repository", &[]).unwrap();
    g.add("architect", "agent", &["inspect"]).unwrap();
    g.add("implement", "agent", &["architect"]).unwrap();
    g.add("tests", "verification", &["implement"]).unwrap();
    g.add("benchmark", "verification", &["implement"]).unwrap();
    g.add("review", "agent", &["tests", "benchmark"]).unwrap();
    g.add("decision", "decision", &["review"]).unwrap();
    g
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn orders_dependencies() {
        let mut g = default_workflow();
        assert_eq!(g.ready(), vec!["inspect"]);
        g.set_status("inspect", TaskStatus::Completed).unwrap();
        assert_eq!(g.ready(), vec!["architect"]);
    }
    #[test]
    fn rejects_cycles() {
        let mut g = TaskGraph::default();
        g.add("a", "x", &["b"]).unwrap();
        g.add("b", "x", &["a"]).unwrap();
        assert!(g.validate().is_err());
    }
}
