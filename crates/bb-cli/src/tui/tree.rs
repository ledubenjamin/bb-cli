use bb_api::models::{Run, RunState, Step};
use std::collections::{HashMap, HashSet};

use crate::log_cache::{LogIdentity, clean_inline};
use crate::pipeline_status::failed;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Run(String),
    Step { run: String, step: String },
    Earlier(String),
    Cycle,
}

#[derive(Debug, Clone)]
pub struct Row {
    pub key: String,
    pub node: Node,
    pub depth: usize,
    pub label: String,
    pub status: String,
    pub expandable: bool,
    pub open: bool,
    pub failed: bool,
}

#[derive(Debug, Default)]
pub struct Tree {
    pub root: String,
    pub runs: HashMap<String, Run>,
    pub steps: HashMap<String, Vec<Step>>,
    pub earlier: HashMap<(String, String), Step>,
    pub expanded: HashSet<String>,
    pub rows: Vec<Row>,
    pub children_loaded: bool,
    pub relations: HashMap<String, HashMap<String, Vec<String>>>,
}

pub fn status(state: &RunState) -> String {
    clean_inline(
        state
            .result
            .as_ref()
            .map_or(state.name.as_str(), |result| &result.name),
    )
}

impl Tree {
    pub fn new(run: Run) -> Self {
        let root = run.uuid.clone();
        let mut tree = Self {
            root: root.clone(),
            ..Self::default()
        };
        tree.expanded.insert(root.clone());
        tree.runs.insert(root, run);
        tree.rebuild();
        tree
    }

    pub fn children(&self, parent: &str, step: &str) -> Vec<&Run> {
        self.relations
            .get(parent)
            .and_then(|steps| steps.get(step))
            .into_iter()
            .flatten()
            .filter_map(|uuid| self.runs.get(uuid))
            .collect()
    }

    pub fn missing_triggers(&self, parent: &str) -> Vec<String> {
        let Some(steps) = self.steps.get(parent) else {
            return Vec::new();
        };
        let mut triggers: Vec<_> = self
            .relations
            .get(parent)
            .into_iter()
            .flat_map(HashMap::keys)
            .filter(|uuid| !steps.iter().any(|step| step.uuid == **uuid))
            .cloned()
            .collect();
        triggers.sort();
        triggers
    }

    pub fn step(&self, run: &str, step: &str) -> Option<&Step> {
        self.steps
            .get(run)
            .and_then(|steps| steps.iter().find(|value| value.uuid == step))
            .or_else(|| self.earlier.get(&(run.to_owned(), step.to_owned())))
    }

    pub fn rebuild(&mut self) {
        self.relations.clear();
        for run in self.runs.values() {
            let metadata = &run.trigger.metadata;
            if let (Some(parent), Some(step)) = (&metadata.pipeline_uuid, &metadata.step_uuid) {
                self.relations
                    .entry(parent.uuid.clone())
                    .or_default()
                    .entry(step.uuid.clone())
                    .or_default()
                    .push(run.uuid.clone());
            }
        }
        for steps in self.relations.values_mut() {
            for children in steps.values_mut() {
                children.sort_by_key(|uuid| self.runs.get(uuid).map_or(0, |run| run.build_number));
            }
        }
        let mut rows = Vec::new();
        self.append_run(&self.root, 0, &mut HashSet::new(), &mut rows);
        self.rows = rows;
    }

    fn append_run(
        &self,
        uuid: &str,
        depth: usize,
        path: &mut HashSet<String>,
        rows: &mut Vec<Row>,
    ) {
        let Some(run) = self.runs.get(uuid) else {
            return;
        };
        if depth > 64 || !path.insert(uuid.to_owned()) {
            rows.push(Row {
                key: format!("cycle:{uuid}"),
                node: Node::Cycle,
                depth,
                label: "Invalid child cycle".into(),
                status: String::new(),
                expandable: false,
                open: false,
                failed: false,
            });
            return;
        }
        let open = self.expanded.contains(uuid);
        rows.push(Row {
            key: uuid.to_owned(),
            node: Node::Run(uuid.to_owned()),
            depth,
            label: format!(
                "#{} · {} · attempt {}",
                run.build_number,
                clean_inline(&run.target.ref_name),
                run.run_number
            ),
            status: status(&run.state),
            expandable: true,
            open,
            failed: failed(&run.state),
        });
        if open {
            if let Some(steps) = self.steps.get(uuid) {
                for step in steps {
                    self.append_step(uuid, &step.uuid, Some(step), depth + 1, path, rows);
                }
            }
            let missing = self.missing_triggers(uuid);
            if !missing.is_empty() {
                let key = format!("earlier:{uuid}");
                let open = self.expanded.contains(&key);
                rows.push(Row {
                    key,
                    node: Node::Earlier(uuid.to_owned()),
                    depth: depth + 1,
                    label: "Earlier triggers".into(),
                    status: String::new(),
                    expandable: true,
                    open,
                    failed: false,
                });
                if open {
                    for step in missing {
                        self.append_step(
                            uuid,
                            &step,
                            self.step(uuid, &step),
                            depth + 2,
                            path,
                            rows,
                        );
                    }
                }
            }
        }
        path.remove(uuid);
    }

    fn append_step(
        &self,
        run: &str,
        uuid: &str,
        step: Option<&Step>,
        depth: usize,
        path: &mut HashSet<String>,
        rows: &mut Vec<Row>,
    ) {
        let children = self.children(run, uuid);
        let key = format!("{run}:{uuid}");
        let open = self.expanded.contains(&key);
        rows.push(Row {
            key,
            node: Node::Step {
                run: run.into(),
                step: uuid.into(),
            },
            depth,
            label: step.map_or_else(
                || "Earlier trigger (loading…)".into(),
                |step| format!("{} · attempt {}", clean_inline(&step.name), step.run_number),
            ),
            status: step.map_or_else(String::new, |step| status(&step.state)),
            expandable: !children.is_empty(),
            open,
            failed: step.is_some_and(|step| failed(&step.state)),
        });
        if open {
            for child in children {
                self.append_run(&child.uuid, depth + 1, path, rows);
            }
        }
    }

    pub fn expand_failures(&mut self) {
        let failed: Vec<_> = self
            .runs
            .values()
            .filter(|run| failed(&run.state))
            .map(|run| run.uuid.clone())
            .collect();
        for run in failed {
            self.expand_to(&run);
        }
    }

    pub fn expand_to(&mut self, uuid: &str) {
        // Follow references, never names: reruns can reuse names and replace UUIDs.
        let Some(mut current) = self.runs.get(uuid) else {
            return;
        };
        let mut seen = HashSet::new();
        while seen.insert(current.uuid.clone()) {
            self.expanded.insert(current.uuid.clone());
            let parent = &current.trigger.metadata;
            let (Some(parent), Some(step)) = (&parent.pipeline_uuid, &parent.step_uuid) else {
                break;
            };
            self.expanded
                .insert(format!("{}:{}", parent.uuid, step.uuid));
            self.expanded.insert(format!("earlier:{}", parent.uuid));
            let Some(next) = self.runs.get(&parent.uuid) else {
                break;
            };
            current = next;
        }
    }

    pub fn descends_from(&self, ancestor: &str, run: &str) -> bool {
        let mut current = run;
        let mut seen = HashSet::new();
        while seen.insert(current) {
            if current == ancestor {
                return true;
            }
            let Some(parent) = self
                .runs
                .get(current)
                .and_then(|run| run.trigger.metadata.pipeline_uuid.as_ref())
            else {
                break;
            };
            current = &parent.uuid;
        }
        false
    }

    pub fn identity(&self, repository: &str, node: &Node) -> Option<LogIdentity> {
        let Node::Step { run, step } = node else {
            return None;
        };
        let pipeline = self.runs.get(run)?;
        let step = self.step(run, step)?;
        let mut path = vec![clean_inline(&step.name)];
        let mut current = pipeline;
        let mut seen = HashSet::new();
        while seen.insert(current.uuid.clone()) {
            path.push(format!(
                "#{} (attempt {})",
                current.build_number, current.run_number
            ));
            if current.uuid == self.root {
                break;
            }
            let Some(parent) = &current.trigger.metadata.pipeline_uuid else {
                break;
            };
            if let Some(trigger) = &current.trigger.metadata.step_uuid {
                path.push(self.step(&parent.uuid, &trigger.uuid).map_or_else(
                    || "Earlier trigger".into(),
                    |value| clean_inline(&value.name),
                ));
            }
            let Some(next) = self.runs.get(&parent.uuid) else {
                break;
            };
            current = next;
        }
        path.reverse();
        Some(LogIdentity {
            workspace: repository.split('/').next()?.into(),
            repository: repository.into(),
            root_run_uuid: self.root.clone(),
            parent_run_uuid: pipeline
                .trigger
                .metadata
                .pipeline_uuid
                .as_ref()
                .map(|value| value.uuid.clone()),
            parent_run_attempt_uuid: pipeline.trigger.metadata.pipeline_run_uuid.clone(),
            parent_step_uuid: pipeline
                .trigger
                .metadata
                .step_uuid
                .as_ref()
                .map(|value| value.uuid.clone()),
            run_uuid: run.clone(),
            run_number: pipeline.build_number,
            run_attempt: pipeline.run_number,
            step_uuid: step.uuid.clone(),
            step_name: clean_inline(&step.name),
            step_attempt: step.run_number,
            execution_path: path,
        })
    }
}
