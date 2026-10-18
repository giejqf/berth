//! Event-flow layout (AGENTS.md §5.4, §11). One column: main-lane nodes in
//! order, subagents spawned together as one group block (a row per subagent,
//! its decisions and result inside), and a marker where each section ends.
//! Views only draw what this computes.

use std::collections::HashMap;

use berth_protocol::NodePayload;

use crate::model::{BranchState, WorkflowView};

/// What a row draws.
#[derive(Debug, Clone, PartialEq)]
pub enum FlowItem {
    /// `view.nodes[index]` on the main lane.
    Node { index: usize },
    /// Subagents spawned from one assistant message (or a lone spawn).
    Subagents(SubagentGroup),
    /// After a Summary: section `section_no` ends here.
    SectionEnd { section_no: u32 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubagentGroup {
    /// The parallel group id, or the lone branch's id.
    pub key: String,
    pub lanes: Vec<Lane>,
    /// When the first subagent was spawned (ms).
    pub started_at: i64,
    /// When the last one was collected, once all are.
    pub collected_at: Option<i64>,
}

impl SubagentGroup {
    #[must_use]
    pub fn running(&self) -> usize {
        self.lanes
            .iter()
            .filter(|l| l.status == BranchState::Running)
            .count()
    }
}

/// One subagent in a group.
#[derive(Debug, Clone, PartialEq)]
pub struct Lane {
    pub branch_id: String,
    /// Position in the group: picks the lane color (cycles every 3).
    pub position: usize,
    pub agent_name: String,
    pub description: String,
    /// `view.nodes` indices of the subagent's Decision nodes.
    pub decisions: Vec<usize>,
    /// `view.nodes` index of its CollectResults node, once collected.
    pub collect: Option<usize>,
    pub files: usize,
    pub tool_calls: u32,
    pub duration_ms: Option<u64>,
    pub status: BranchState,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FlowRow {
    pub item: FlowItem,
    /// Section the row belongs to (1-based).
    pub section_no: u32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FlowLayout {
    pub rows: Vec<FlowRow>,
}

/// Lay out a workflow.
#[must_use]
pub fn layout(view: &WorkflowView) -> FlowLayout {
    let mut out = FlowLayout::default();
    let mut section_no = 1;
    // group key -> row index
    let mut groups: HashMap<String, usize> = HashMap::new();
    // branch id -> (row index, lane position)
    let mut lanes: HashMap<String, (usize, usize)> = HashMap::new();

    for (index, node) in view.nodes.iter().enumerate() {
        if let Some(branch) = node.branch_id.as_deref() {
            // Inside a subagent: its decisions belong to its lane.
            if let (NodePayload::Decision(_), Some(&(row, pos))) =
                (&node.payload, lanes.get(branch))
                && let FlowItem::Subagents(g) = &mut out.rows[row].item
            {
                g.lanes[pos].decisions.push(index);
            }
            continue;
        }
        match &node.payload {
            NodePayload::SubagentSpawn(s) => {
                let key = s
                    .parallel_group
                    .clone()
                    .unwrap_or_else(|| s.branch_id.clone());
                let branch = view.branch(&s.branch_id);
                let row = *groups.entry(key.clone()).or_insert_with(|| {
                    out.rows.push(FlowRow {
                        item: FlowItem::Subagents(SubagentGroup {
                            key,
                            lanes: Vec::new(),
                            started_at: node.created_at,
                            collected_at: None,
                        }),
                        section_no,
                    });
                    out.rows.len() - 1
                });
                if let FlowItem::Subagents(g) = &mut out.rows[row].item {
                    let position = g.lanes.len();
                    let result = branch.and_then(|b| b.result.as_ref());
                    g.lanes.push(Lane {
                        branch_id: s.branch_id.clone(),
                        position,
                        agent_name: s.agent_name.clone(),
                        description: s.description.clone(),
                        decisions: Vec::new(),
                        collect: None,
                        files: result.map_or(0, |r| r.touched_files.len()),
                        tool_calls: result.map_or(0, |r| r.tool_calls),
                        duration_ms: result.map(|r| r.duration_ms),
                        status: branch.map_or(BranchState::Running, |b| b.status),
                    });
                    lanes.insert(s.branch_id.clone(), (row, position));
                }
            }
            NodePayload::CollectResults(c) => {
                if let Some(&(row, pos)) = lanes.get(&c.branch_id)
                    && let FlowItem::Subagents(g) = &mut out.rows[row].item
                {
                    let lane = &mut g.lanes[pos];
                    lane.collect = Some(index);
                    lane.files = c.touched_files.len();
                    lane.tool_calls = c.tool_calls;
                    lane.duration_ms = Some(c.duration_ms);
                    if lane.status == BranchState::Running {
                        lane.status = match c.status {
                            berth_protocol::BranchStatus::Ok => BranchState::Ok,
                            berth_protocol::BranchStatus::Error => BranchState::Error,
                            berth_protocol::BranchStatus::Cancelled => BranchState::Cancelled,
                        };
                    }
                    if g.lanes.iter().all(|l| l.collect.is_some()) {
                        g.collected_at = Some(node.created_at);
                    }
                }
            }
            payload => {
                out.rows.push(FlowRow {
                    item: FlowItem::Node { index },
                    section_no,
                });
                if matches!(payload, NodePayload::Summary(_)) {
                    out.rows.push(FlowRow {
                        item: FlowItem::SectionEnd { section_no },
                        section_no,
                    });
                    section_no += 1;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::model::{Branch, Node, Workflow, WorkflowState};
    use berth_protocol::{
        BranchStatus, CollectResults, Decision, Origin, OriginKind, Prompt, SubagentSpawn, Summary,
    };

    fn node(seq: i64, branch: Option<&str>, payload: NodePayload) -> Node {
        Node {
            id: format!("n{seq}"),
            workflow_id: "w".into(),
            round_id: None,
            branch_id: branch.map(str::to_owned),
            seq,
            payload,
            raw_first: None,
            raw_last: None,
            created_at: 0,
        }
    }

    fn decision(t: &str) -> NodePayload {
        NodePayload::Decision(Decision {
            title: t.into(),
            rationale: String::new(),
            affects: vec![],
            reversible: true,
        })
    }

    fn spawn(b: &str) -> NodePayload {
        NodePayload::SubagentSpawn(SubagentSpawn {
            branch_id: b.into(),
            agent_name: "explore".into(),
            description: "d".into(),
            prompt: "p".into(),
            parallel_group: Some("g".into()),
        })
    }

    fn collect(b: &str) -> NodePayload {
        NodePayload::CollectResults(CollectResults {
            branch_id: b.into(),
            result_text: String::new(),
            duration_ms: 41_000,
            cost_usd: None,
            tool_calls: 3,
            touched_files: vec!["a".into(), "b".into()],
            status: BranchStatus::Ok,
        })
    }

    fn branch(id: &str, done: bool) -> Branch {
        Branch {
            id: id.into(),
            workflow_id: "w".into(),
            spawn_round_id: None,
            collect_round_id: None,
            parent_branch_id: None,
            parent_tool_use_id: format!("tu-{id}"),
            agent_name: "explore".into(),
            description: "d".into(),
            parallel_group: None,
            status: if done {
                BranchState::Ok
            } else {
                BranchState::Running
            },
            spawned_at: 0,
            collected_at: None,
            cost_usd: None,
            spawn_tree: None,
            collect_tree: None,
            result: done.then(|| match collect(id) {
                NodePayload::CollectResults(c) => c,
                _ => unreachable!(),
            }),
        }
    }

    fn view(nodes: Vec<Node>, branches: Vec<Branch>) -> WorkflowView {
        WorkflowView {
            workflow: Workflow {
                id: "w".into(),
                slot_id: "s".into(),
                origin: Origin {
                    kind: OriginKind::Thought,
                    r#ref: String::new(),
                    title: "t".into(),
                    body: String::new(),
                    custom_prompt: None,
                },
                state: WorkflowState::Running,
                claude_session_id: None,
                section_no: 1,
                total_cost_usd: 0.0,
                last_error: None,
                created_at: 0,
                closed_at: None,
            },
            nodes,
            branches,
            rounds: vec![],
            snapshots: vec![],
        }
    }

    fn parallel_view() -> WorkflowView {
        view(
            vec![
                node(
                    1,
                    None,
                    NodePayload::Prompt(Prompt {
                        text: "go".into(),
                        review_comments: vec![],
                        attachments: vec![],
                    }),
                ),
                node(2, None, spawn("a")),
                node(3, None, spawn("b")),
                node(4, Some("a"), decision("da1")),
                node(5, Some("b"), decision("db1")),
                node(6, Some("a"), decision("da2")),
                node(7, None, collect("a")),
                node(8, None, decision("main")),
                node(9, None, collect("b")),
                node(
                    10,
                    None,
                    NodePayload::Summary(Summary {
                        done: vec![],
                        remaining: vec![],
                        follow_ups: vec![],
                        notes: None,
                    }),
                ),
                node(11, None, decision("next section")),
            ],
            vec![branch("a", true), branch("b", true)],
        )
    }

    #[test]
    fn parallel_subagents_form_one_group_with_their_decisions() {
        let l = layout(&parallel_view());
        let kinds: Vec<&str> = l
            .rows
            .iter()
            .map(|r| match &r.item {
                FlowItem::Node { .. } => "node",
                FlowItem::Subagents(_) => "group",
                FlowItem::SectionEnd { .. } => "end",
            })
            .collect();
        assert_eq!(kinds, vec!["node", "group", "node", "node", "end", "node"]);
        let FlowItem::Subagents(g) = &l.rows[1].item else {
            panic!()
        };
        assert_eq!(g.lanes.len(), 2);
        assert_eq!(g.lanes[0].decisions, vec![3, 5]);
        assert_eq!(g.lanes[1].decisions, vec![4]);
        assert_eq!(g.lanes[0].collect, Some(6));
        assert_eq!((g.lanes[0].files, g.lanes[0].tool_calls), (2, 3));
        assert_eq!(g.lanes[1].position, 1);
        assert_eq!(g.running(), 0);
        assert!(g.collected_at.is_some());
        // Sections advance after a Summary; the marker belongs to the old one.
        assert_eq!(l.rows[4].item, FlowItem::SectionEnd { section_no: 1 });
        assert_eq!(l.rows.last().unwrap().section_no, 2);
    }

    #[test]
    fn a_running_subagent_keeps_its_group_open() {
        let v = view(
            vec![node(1, None, spawn("a")), node(2, Some("a"), decision("d"))],
            vec![branch("a", false)],
        );
        let l = layout(&v);
        let FlowItem::Subagents(g) = &l.rows[0].item else {
            panic!()
        };
        assert_eq!(g.running(), 1);
        assert_eq!(g.collected_at, None);
        assert_eq!(g.lanes[0].decisions, vec![1]);
    }
}
