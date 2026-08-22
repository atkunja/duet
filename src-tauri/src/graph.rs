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
        let mut ready = self
            .nodes
            .values()
            .filter(|node| {
                node.status == TaskStatus::Pending
                    && node.dependencies.iter().all(|dep| {
                        self.nodes.get(dep).is_some_and(|n| {
                            matches!(n.status, TaskStatus::Completed | TaskStatus::Skipped)
                        })
                    })
            })
            .map(|n| n.id.clone())
            .collect::<Vec<_>>();
        ready.sort();
        ready
    }

    pub fn set_status(&mut self, id: &str, status: TaskStatus) -> Result<()> {
        self.nodes
            .get_mut(id)
            .ok_or_else(|| anyhow!("unknown task node {id}"))?
            .status = status;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct RepairRoundNodes {
    pub repair: String,
    pub tests: String,
    pub benchmark: String,
    pub review: String,
    pub decision: String,
}

/// Enforces dependency transitions while the workflow performs each node's work.
/// A node can only start when every dependency has completed or been skipped.
pub struct TaskExecutor {
    graph: TaskGraph,
}

impl TaskExecutor {
    pub fn new(graph: TaskGraph) -> Result<Self> {
        graph.validate()?;
        Ok(Self { graph })
    }

    pub fn start(&mut self, id: &str) -> Result<()> {
        if !self.graph.ready().iter().any(|ready| ready == id) {
            return Err(anyhow!("task node {id} is not ready"));
        }
        self.graph.set_status(id, TaskStatus::Running)
    }

    pub fn complete(&mut self, id: &str) -> Result<()> {
        self.transition_running(id, TaskStatus::Completed)
    }

    pub fn fail(&mut self, id: &str) -> Result<()> {
        self.transition_running(id, TaskStatus::Failed)
    }

    pub fn skip(&mut self, id: &str) -> Result<()> {
        if !self.graph.ready().iter().any(|ready| ready == id) {
            return Err(anyhow!("task node {id} is not ready to skip"));
        }
        self.graph.set_status(id, TaskStatus::Skipped)
    }

    pub fn add_repair_round(
        &mut self,
        round: u8,
        previous_decision: &str,
    ) -> Result<RepairRoundNodes> {
        let nodes = RepairRoundNodes {
            repair: format!("repair-{round}"),
            tests: format!("tests-{round}"),
            benchmark: format!("benchmark-{round}"),
            review: format!("review-{round}"),
            decision: format!("decision-{round}"),
        };
        self.graph
            .add(&nodes.repair, "agent", &[previous_decision])?;
        self.graph
            .add(&nodes.tests, "verification", &[&nodes.repair])?;
        self.graph
            .add(&nodes.benchmark, "verification", &[&nodes.repair])?;
        self.graph
            .add(&nodes.review, "agent", &[&nodes.tests, &nodes.benchmark])?;
        self.graph
            .add(&nodes.decision, "decision", &[&nodes.review])?;
        self.graph.validate()?;
        Ok(nodes)
    }

    fn transition_running(&mut self, id: &str, status: TaskStatus) -> Result<()> {
        let node = self
            .graph
            .nodes
            .get(id)
            .ok_or_else(|| anyhow!("unknown task node {id}"))?;
        if node.status != TaskStatus::Running {
            return Err(anyhow!("task node {id} is not running"));
        }
        self.graph.set_status(id, status)
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

    #[test]
    fn executor_enforces_dependencies_and_expands_repair_rounds() {
        let mut executor = TaskExecutor::new(default_workflow()).unwrap();
        assert!(executor.start("architect").is_err());
        executor.start("inspect").unwrap();
        executor.complete("inspect").unwrap();
        executor.start("architect").unwrap();
        executor.complete("architect").unwrap();
        executor.start("implement").unwrap();
        executor.complete("implement").unwrap();
        executor.start("tests").unwrap();
        executor.start("benchmark").unwrap();
        executor.complete("tests").unwrap();
        executor.skip("benchmark").unwrap_err();
        executor.complete("benchmark").unwrap();
        executor.start("review").unwrap();
        executor.complete("review").unwrap();
        executor.start("decision").unwrap();
        executor.complete("decision").unwrap();
        let round = executor.add_repair_round(1, "decision").unwrap();
        executor.start(&round.repair).unwrap();
        executor.complete(&round.repair).unwrap();
        executor.skip(&round.benchmark).unwrap();
        executor.start(&round.tests).unwrap();
        executor.complete(&round.tests).unwrap();
        executor.start(&round.review).unwrap();
    }
}
