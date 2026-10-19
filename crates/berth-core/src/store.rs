//! SQLite persistence (AGENTS.md §9). One database, embedded migrations.

use std::path::Path;

use berth_protocol::{
    CollectResults, DiffPreset, DiffSide, NodeKind, NodePayload, Origin, TerminalKind,
};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::error::{CoreError, Result};
use crate::ids::{new_id, now_ms};
use crate::model::{
    Branch, BranchState, ConnectorRow, GhEntityRow, Node, RawEventRow, Repo, ReviewCommentRow,
    Round, Slot, SlotState, Snapshot, SnapshotKind, Workflow, WorkflowFacts, WorkflowState,
    WorkflowView,
};

const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_init.sql"),
    include_str!("../migrations/0002_slot_custom_name.sql"),
];

pub struct Store {
    conn: Connection,
}

fn corrupt(what: &'static str, value: impl Into<String>) -> CoreError {
    CoreError::Corrupt {
        what,
        value: value.into(),
    }
}

fn not_found(kind: &'static str, id: &str) -> CoreError {
    CoreError::NotFound {
        kind,
        id: id.to_owned(),
    }
}

fn u32_col(v: i64) -> u32 {
    u32::try_from(v).unwrap_or(0)
}

const SLOT_COLS: &str = "id, repo_id, type, name, state, state_detail, checkout_path, placement, branch, base_ref, created_at, custom_name";
const WORKFLOW_COLS: &str = "id, slot_id, origin_json, state, claude_session_id, section_no, total_cost_usd, \
                             last_error, created_at, closed_at";
const ROUND_COLS: &str = "id, workflow_id, section_no, started_at, ended_at, terminal_kind, nudges, cost_usd, duration_ms";
const BRANCH_COLS: &str = "id, workflow_id, spawn_round_id, collect_round_id, parent_branch_id, \
                           parent_tool_use_id, agent_name, description, parallel_group, status, spawned_at, \
                           collected_at, cost_usd, spawn_tree, collect_tree, result_json";
const NODE_COLS: &str = "id, workflow_id, round_id, branch_id, seq, kind, payload_json, raw_first, raw_last, created_at";
const COMMENT_COLS: &str = "id, workflow_id, from_snapshot, to_snapshot, preset, path, side, line, body, \
                            sent_in_node, posted_to_pr, created_at";

fn slot_row(r: &Row<'_>) -> rusqlite::Result<(Slot, String)> {
    let state: String = r.get(4)?;
    Ok((
        Slot {
            id: r.get(0)?,
            repo_id: r.get(1)?,
            slot_type: r.get(2)?,
            name: r.get(3)?,
            state: SlotState::Failed,
            state_detail: r.get(5)?,
            checkout_path: r.get(6)?,
            placement: r.get(7)?,
            branch: r.get(8)?,
            base_ref: r.get(9)?,
            created_at: r.get(10)?,
            custom_name: r.get(11)?,
        },
        state,
    ))
}

fn finish_slot((mut slot, state): (Slot, String)) -> Result<Slot> {
    slot.state = SlotState::parse(&state).ok_or_else(|| corrupt("slot state", state))?;
    Ok(slot)
}

type WorkflowRaw = (
    String,
    String,
    String,
    String,
    Option<String>,
    i64,
    f64,
    Option<String>,
    i64,
    Option<i64>,
);

fn workflow_row(r: &Row<'_>) -> rusqlite::Result<WorkflowRaw> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
        r.get(8)?,
        r.get(9)?,
    ))
}

fn finish_workflow(raw: WorkflowRaw) -> Result<Workflow> {
    let (
        id,
        slot_id,
        origin_json,
        state,
        claude_session_id,
        section_no,
        total_cost_usd,
        last_error,
        created_at,
        closed_at,
    ) = raw;
    Ok(Workflow {
        origin: serde_json::from_str::<Origin>(&origin_json)?,
        state: WorkflowState::parse(&state).ok_or_else(|| corrupt("workflow state", state))?,
        id,
        slot_id,
        claude_session_id,
        section_no: u32_col(section_no),
        total_cost_usd,
        last_error,
        created_at,
        closed_at,
    })
}

fn round_row(r: &Row<'_>) -> rusqlite::Result<(Round, Option<String>)> {
    Ok((
        Round {
            id: r.get(0)?,
            workflow_id: r.get(1)?,
            section_no: u32_col(r.get(2)?),
            started_at: r.get(3)?,
            ended_at: r.get(4)?,
            terminal_kind: None,
            nudges: u32_col(r.get(6)?),
            cost_usd: r.get(7)?,
            duration_ms: r.get(8)?,
        },
        r.get(5)?,
    ))
}

fn finish_round((mut round, kind): (Round, Option<String>)) -> Result<Round> {
    round.terminal_kind = match kind {
        Some(k) => Some(TerminalKind::parse(&k).ok_or_else(|| corrupt("terminal kind", k))?),
        None => None,
    };
    Ok(round)
}

fn branch_row(r: &Row<'_>) -> rusqlite::Result<(Branch, String, Option<String>)> {
    Ok((
        Branch {
            id: r.get(0)?,
            workflow_id: r.get(1)?,
            spawn_round_id: r.get(2)?,
            collect_round_id: r.get(3)?,
            parent_branch_id: r.get(4)?,
            parent_tool_use_id: r.get(5)?,
            agent_name: r.get(6)?,
            description: r.get(7)?,
            parallel_group: r.get(8)?,
            status: BranchState::Running,
            spawned_at: r.get(10)?,
            collected_at: r.get(11)?,
            cost_usd: r.get(12)?,
            spawn_tree: r.get(13)?,
            collect_tree: r.get(14)?,
            result: None,
        },
        r.get(9)?,
        r.get(15)?,
    ))
}

fn finish_branch((mut b, status, result): (Branch, String, Option<String>)) -> Result<Branch> {
    b.status = BranchState::parse(&status).ok_or_else(|| corrupt("branch status", status))?;
    b.result = match result {
        Some(json) => Some(serde_json::from_str::<CollectResults>(&json)?),
        None => None,
    };
    Ok(b)
}

type NodeRaw = (
    String,
    String,
    Option<String>,
    Option<String>,
    i64,
    String,
    String,
    Option<i64>,
    Option<i64>,
    i64,
);

fn node_row(r: &Row<'_>) -> rusqlite::Result<NodeRaw> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
        r.get(8)?,
        r.get(9)?,
    ))
}

fn finish_node(raw: NodeRaw) -> Result<Node> {
    let (
        id,
        workflow_id,
        round_id,
        branch_id,
        seq,
        kind,
        payload_json,
        raw_first,
        raw_last,
        created_at,
    ) = raw;
    let kind = NodeKind::parse(&kind).ok_or_else(|| corrupt("node kind", kind))?;
    Ok(Node {
        payload: NodePayload::from_parts(kind, &payload_json)?,
        id,
        workflow_id,
        round_id,
        branch_id,
        seq,
        raw_first,
        raw_last,
        created_at,
    })
}

fn snapshot_row(r: &Row<'_>) -> rusqlite::Result<(Snapshot, String)> {
    Ok((
        Snapshot {
            id: r.get(0)?,
            workflow_id: r.get(1)?,
            section_no: u32_col(r.get(2)?),
            kind: SnapshotKind::Start,
            tree_sha: r.get(4)?,
            created_at: r.get(5)?,
        },
        r.get(3)?,
    ))
}

fn finish_snapshot((mut s, kind): (Snapshot, String)) -> Result<Snapshot> {
    s.kind = SnapshotKind::parse(&kind).ok_or_else(|| corrupt("snapshot kind", kind))?;
    Ok(s)
}

type CommentRaw = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    String,
    Option<String>,
    bool,
    i64,
);

fn comment_row(r: &Row<'_>) -> rusqlite::Result<CommentRaw> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
        r.get(8)?,
        r.get(9)?,
        r.get(10)?,
        r.get(11)?,
    ))
}

fn finish_comment(raw: CommentRaw) -> Result<ReviewCommentRow> {
    let (
        id,
        workflow_id,
        from_snapshot,
        to_snapshot,
        preset,
        path,
        side,
        line,
        body,
        sent_in_node,
        posted_to_pr,
        created_at,
    ) = raw;
    let preset: DiffPreset = serde_json::from_value(serde_json::Value::String(preset))?;
    let side: DiffSide = serde_json::from_value(serde_json::Value::String(side))?;
    Ok(ReviewCommentRow {
        id,
        workflow_id,
        from_snapshot,
        to_snapshot,
        preset,
        path,
        side,
        line: u32_col(line),
        body,
        sent_in_node,
        posted_to_pr,
        created_at,
    })
}

fn collect<T, R>(
    rows: impl Iterator<Item = rusqlite::Result<R>>,
    finish: impl Fn(R) -> Result<T>,
) -> Result<Vec<T>> {
    rows.map(|r| finish(r?)).collect()
}

/// Fields for a new review comment.
#[derive(Debug, Clone)]
pub struct NewReviewComment {
    pub workflow_id: String,
    pub from_snapshot: String,
    pub to_snapshot: String,
    pub preset: DiffPreset,
    pub path: String,
    pub side: DiffSide,
    pub line: u32,
    pub body: String,
}

/// Fields for a new raw event row.
#[derive(Debug, Clone)]
pub struct NewRawEvent<'a> {
    pub workflow_id: &'a str,
    pub round_id: Option<&'a str>,
    pub branch_id: Option<&'a str>,
    pub seq: i64,
    pub kind: &'a str,
    pub json: &'a str,
}

impl Store {
    /// Open (creating if needed) and migrate.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let mut store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<()> {
        let version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let applied = usize::try_from(version).unwrap_or(0);
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(applied) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(
                None,
                "user_version",
                i64::try_from(i + 1).unwrap_or(i64::MAX),
            )?;
            tx.commit()?;
        }
        Ok(())
    }

    /// Run `f` in a transaction.
    pub fn transaction<T>(&mut self, f: impl FnOnce(&Store) -> Result<T>) -> Result<T> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        match f(self) {
            Ok(v) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(v)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    // ---- repos ----

    /// Find or create the repo at `path`.
    pub fn upsert_repo(&self, path: &Path) -> Result<Repo> {
        if let Some(repo) = self.repo_by_path(path)? {
            return Ok(repo);
        }
        let repo = Repo {
            id: new_id(),
            path: path.to_path_buf(),
            default_slot_type: None,
            created_at: now_ms(),
        };
        self.conn.execute(
            "INSERT INTO repos (id, path, default_slot_type, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                repo.id,
                repo.path.to_string_lossy(),
                repo.default_slot_type,
                repo.created_at
            ],
        )?;
        Ok(repo)
    }

    pub fn repo_by_path(&self, path: &Path) -> Result<Option<Repo>> {
        self.conn
            .query_row(
                "SELECT id, path, default_slot_type, created_at FROM repos WHERE path = ?1",
                [path.to_string_lossy()],
                |r| {
                    Ok(Repo {
                        id: r.get(0)?,
                        path: r.get::<_, String>(1)?.into(),
                        default_slot_type: r.get(2)?,
                        created_at: r.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn repo(&self, id: &str) -> Result<Repo> {
        self.conn
            .query_row(
                "SELECT id, path, default_slot_type, created_at FROM repos WHERE id = ?1",
                [id],
                |r| {
                    Ok(Repo {
                        id: r.get(0)?,
                        path: r.get::<_, String>(1)?.into(),
                        default_slot_type: r.get(2)?,
                        created_at: r.get(3)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| not_found("repo", id))
    }

    pub fn set_repo_default_slot(&self, id: &str, slot_type: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE repos SET default_slot_type = ?2 WHERE id = ?1",
            params![id, slot_type],
        )?;
        Ok(())
    }

    // ---- slots ----

    pub fn insert_slot(&self, s: &Slot) -> Result<()> {
        self.conn.execute(
            &format!("INSERT INTO slots ({SLOT_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"),
            params![
                s.id,
                s.repo_id,
                s.slot_type,
                s.name,
                s.state.as_str(),
                s.state_detail,
                s.checkout_path,
                s.placement,
                s.branch,
                s.base_ref,
                s.created_at,
                s.custom_name
            ],
        )?;
        Ok(())
    }

    pub fn slot(&self, id: &str) -> Result<Slot> {
        let raw = self
            .conn
            .query_row(
                &format!("SELECT {SLOT_COLS} FROM slots WHERE id = ?1"),
                [id],
                slot_row,
            )
            .optional()?
            .ok_or_else(|| not_found("slot", id))?;
        finish_slot(raw)
    }

    /// Slots of a repo that are not destroyed, oldest first.
    pub fn live_slots(&self, repo_id: &str) -> Result<Vec<Slot>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SLOT_COLS} FROM slots WHERE repo_id = ?1 AND state != 'destroyed' ORDER BY created_at"
        ))?;
        collect(stmt.query_map([repo_id], slot_row)?, finish_slot)
    }

    /// Move a slot along its lifecycle (§6.3); rejects invalid edges.
    pub fn transition_slot(&self, id: &str, to: SlotState, detail: Option<&str>) -> Result<Slot> {
        let slot = self.slot(id)?;
        if slot.state != to && !slot.state.can_transition(to) {
            return Err(CoreError::InvalidTransition(format!(
                "slot {id}: {} -> {to}",
                slot.state
            )));
        }
        self.conn.execute(
            "UPDATE slots SET state = ?2, state_detail = ?3 WHERE id = ?1",
            params![id, to.as_str(), detail],
        )?;
        self.slot(id)
    }

    /// Rename the slot, or with `None` go back to showing its workflow's title.
    pub fn set_slot_custom_name(&self, id: &str, name: Option<&str>) -> Result<Slot> {
        let n = self.conn.execute(
            "UPDATE slots SET custom_name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        if n == 0 {
            return Err(not_found("slot", id));
        }
        self.slot(id)
    }

    pub fn set_slot_checkout(&self, id: &str, path: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE slots SET checkout_path = ?2 WHERE id = ?1",
            params![id, path],
        )?;
        Ok(())
    }

    // ---- workflows ----

    pub fn insert_workflow(&self, w: &Workflow) -> Result<()> {
        self.conn.execute(
            &format!("INSERT INTO workflows ({WORKFLOW_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"),
            params![
                w.id,
                w.slot_id,
                serde_json::to_string(&w.origin)?,
                w.state.as_str(),
                w.claude_session_id,
                w.section_no,
                w.total_cost_usd,
                w.last_error,
                w.created_at,
                w.closed_at
            ],
        )?;
        Ok(())
    }

    pub fn workflow(&self, id: &str) -> Result<Workflow> {
        let raw = self
            .conn
            .query_row(
                &format!("SELECT {WORKFLOW_COLS} FROM workflows WHERE id = ?1"),
                [id],
                workflow_row,
            )
            .optional()?
            .ok_or_else(|| not_found("workflow", id))?;
        finish_workflow(raw)
    }

    /// All workflows of a slot, oldest first.
    pub fn workflows_for_slot(&self, slot_id: &str) -> Result<Vec<Workflow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {WORKFLOW_COLS} FROM workflows WHERE slot_id = ?1 ORDER BY created_at"
        ))?;
        collect(stmt.query_map([slot_id], workflow_row)?, finish_workflow)
    }

    /// The slot's open workflow; a slot hosts at most one (§2).
    pub fn active_workflow(&self, slot_id: &str) -> Result<Option<Workflow>> {
        Ok(self
            .workflows_for_slot(slot_id)?
            .into_iter()
            .rev()
            .find(|w| w.state.is_open()))
    }

    /// Open workflows across all repos (for resume after restart).
    pub fn open_workflows(&self) -> Result<Vec<Workflow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {WORKFLOW_COLS} FROM workflows WHERE state != 'closed' ORDER BY created_at"
        ))?;
        collect(stmt.query_map([], workflow_row)?, finish_workflow)
    }

    pub fn set_workflow_state(&self, id: &str, state: WorkflowState) -> Result<()> {
        let closed_at = (state == WorkflowState::Closed).then(now_ms);
        self.conn.execute(
            "UPDATE workflows SET state = ?2, closed_at = COALESCE(?3, closed_at) WHERE id = ?1",
            params![id, state.as_str(), closed_at],
        )?;
        Ok(())
    }

    pub fn set_claude_session(&self, id: &str, session: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE workflows SET claude_session_id = ?2 WHERE id = ?1",
            params![id, session],
        )?;
        Ok(())
    }

    pub fn set_workflow_error(&self, id: &str, error: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE workflows SET last_error = ?2 WHERE id = ?1",
            params![id, error],
        )?;
        Ok(())
    }

    pub fn set_workflow_cost(&self, id: &str, total_usd: f64) -> Result<()> {
        self.conn.execute(
            "UPDATE workflows SET total_cost_usd = ?2 WHERE id = ?1",
            params![id, total_usd],
        )?;
        Ok(())
    }

    pub fn set_section_no(&self, id: &str, section_no: u32) -> Result<()> {
        self.conn.execute(
            "UPDATE workflows SET section_no = ?2 WHERE id = ?1",
            params![id, section_no],
        )?;
        Ok(())
    }

    // ---- rounds ----

    pub fn insert_round(
        &self,
        workflow_id: &str,
        section_no: u32,
        started_at: i64,
    ) -> Result<Round> {
        let round = Round {
            id: new_id(),
            workflow_id: workflow_id.to_owned(),
            section_no,
            started_at,
            ended_at: None,
            terminal_kind: None,
            nudges: 0,
            cost_usd: 0.0,
            duration_ms: None,
        };
        self.conn.execute(
            "INSERT INTO rounds (id, workflow_id, section_no, started_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                round.id,
                round.workflow_id,
                round.section_no,
                round.started_at
            ],
        )?;
        Ok(round)
    }

    pub fn end_round(
        &self,
        id: &str,
        kind: TerminalKind,
        nudges: u32,
        cost_usd: f64,
        duration_ms: i64,
        ended_at: i64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE rounds SET ended_at = ?2, terminal_kind = ?3, nudges = ?4, cost_usd = ?5, duration_ms = ?6 \
             WHERE id = ?1",
            params![id, ended_at, kind.as_str(), nudges, cost_usd, duration_ms],
        )?;
        Ok(())
    }

    /// The workflow's unfinished round, if any.
    pub fn open_round(&self, workflow_id: &str) -> Result<Option<Round>> {
        let raw = self
            .conn
            .query_row(
                &format!(
                    "SELECT {ROUND_COLS} FROM rounds WHERE workflow_id = ?1 AND ended_at IS NULL \
                     ORDER BY started_at DESC LIMIT 1"
                ),
                [workflow_id],
                round_row,
            )
            .optional()?;
        raw.map(finish_round).transpose()
    }

    pub fn rounds(&self, workflow_id: &str) -> Result<Vec<Round>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {ROUND_COLS} FROM rounds WHERE workflow_id = ?1 ORDER BY started_at"
        ))?;
        collect(stmt.query_map([workflow_id], round_row)?, finish_round)
    }

    // ---- branches ----

    pub fn insert_branch(&self, b: &Branch) -> Result<()> {
        self.conn.execute(
            &format!(
                "INSERT INTO branches ({BRANCH_COLS}) VALUES \
                 (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)"
            ),
            params![
                b.id,
                b.workflow_id,
                b.spawn_round_id,
                b.collect_round_id,
                b.parent_branch_id,
                b.parent_tool_use_id,
                b.agent_name,
                b.description,
                b.parallel_group,
                b.status.as_str(),
                b.spawned_at,
                b.collected_at,
                b.cost_usd,
                b.spawn_tree,
                b.collect_tree,
                b.result.as_ref().map(serde_json::to_string).transpose()?
            ],
        )?;
        Ok(())
    }

    pub fn branch(&self, id: &str) -> Result<Option<Branch>> {
        let raw = self
            .conn
            .query_row(
                &format!("SELECT {BRANCH_COLS} FROM branches WHERE id = ?1"),
                [id],
                branch_row,
            )
            .optional()?;
        raw.map(finish_branch).transpose()
    }

    pub fn branches(&self, workflow_id: &str) -> Result<Vec<Branch>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {BRANCH_COLS} FROM branches WHERE workflow_id = ?1 ORDER BY spawned_at"
        ))?;
        collect(stmt.query_map([workflow_id], branch_row)?, finish_branch)
    }

    pub fn collect_branch(
        &self,
        id: &str,
        result: &CollectResults,
        round_id: Option<&str>,
        collected_at: i64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE branches SET status = ?2, collect_round_id = ?3, collected_at = ?4, cost_usd = ?5, \
             result_json = ?6 WHERE id = ?1",
            params![
                id,
                BranchState::from(result.status).as_str(),
                round_id,
                collected_at,
                result.cost_usd,
                serde_json::to_string(result)?
            ],
        )?;
        Ok(())
    }

    pub fn set_branch_tree(
        &self,
        id: &str,
        spawn: Option<&str>,
        collect: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE branches SET spawn_tree = COALESCE(?2, spawn_tree), collect_tree = COALESCE(?3, collect_tree) \
             WHERE id = ?1",
            params![id, spawn, collect],
        )?;
        Ok(())
    }

    // ---- nodes ----

    /// Append a node to the workflow's flow.
    pub fn insert_node(
        &self,
        workflow_id: &str,
        round_id: Option<&str>,
        branch_id: Option<&str>,
        payload: NodePayload,
        raw_span: Option<(i64, i64)>,
    ) -> Result<Node> {
        let seq: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM nodes WHERE workflow_id = ?1",
            [workflow_id],
            |r| r.get(0),
        )?;
        let node = Node {
            id: new_id(),
            workflow_id: workflow_id.to_owned(),
            round_id: round_id.map(str::to_owned),
            branch_id: branch_id.map(str::to_owned),
            seq,
            payload,
            raw_first: raw_span.map(|s| s.0),
            raw_last: raw_span.map(|s| s.1),
            created_at: now_ms(),
        };
        self.conn.execute(
            &format!(
                "INSERT INTO nodes ({NODE_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"
            ),
            params![
                node.id,
                node.workflow_id,
                node.round_id,
                node.branch_id,
                node.seq,
                node.payload.kind().as_str(),
                node.payload.payload_json()?,
                node.raw_first,
                node.raw_last,
                node.created_at
            ],
        )?;
        Ok(node)
    }

    /// Backdate a node (imports and fixtures).
    pub fn set_node_created_at(&self, id: &str, created_at: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE nodes SET created_at = ?2 WHERE id = ?1",
            params![id, created_at],
        )?;
        Ok(())
    }

    pub fn update_node_payload(&self, id: &str, payload: &NodePayload) -> Result<()> {
        self.conn.execute(
            "UPDATE nodes SET payload_json = ?2 WHERE id = ?1",
            params![id, payload.payload_json()?],
        )?;
        Ok(())
    }

    pub fn node(&self, id: &str) -> Result<Node> {
        let raw = self
            .conn
            .query_row(
                &format!("SELECT {NODE_COLS} FROM nodes WHERE id = ?1"),
                [id],
                node_row,
            )
            .optional()?
            .ok_or_else(|| not_found("node", id))?;
        finish_node(raw)
    }

    pub fn nodes(&self, workflow_id: &str) -> Result<Vec<Node>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {NODE_COLS} FROM nodes WHERE workflow_id = ?1 ORDER BY seq"
        ))?;
        collect(stmt.query_map([workflow_id], node_row)?, finish_node)
    }

    /// What the slot rail says about a workflow without loading its flow.
    pub fn workflow_facts(&self, workflow_id: &str) -> Result<WorkflowFacts> {
        let question_at: Option<i64> = self
            .conn
            .query_row(
                "SELECT created_at FROM nodes WHERE workflow_id = ?1 AND kind = 'question' \
                 AND json_extract(payload_json, '$.answer') IS NULL ORDER BY seq DESC LIMIT 1",
                [workflow_id],
                |r| r.get(0),
            )
            .optional()?;
        let summary_at: Option<i64> = self.conn.query_row(
            "SELECT MAX(created_at) FROM nodes WHERE workflow_id = ?1 AND kind = 'summary'",
            [workflow_id],
            |r| r.get(0),
        )?;
        let outcome: Option<String> = self
            .conn
            .query_row(
                "SELECT payload_json FROM nodes WHERE workflow_id = ?1 AND kind = 'outcome' \
                 ORDER BY seq DESC LIMIT 1",
                [workflow_id],
                |r| r.get(0),
            )
            .optional()?;
        let outcome = match outcome {
            Some(json) => Some(serde_json::from_str(&json)?),
            None => None,
        };
        let rounds: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM rounds WHERE workflow_id = ?1",
            [workflow_id],
            |r| r.get(0),
        )?;
        let last_round_at: Option<i64> = self.conn.query_row(
            "SELECT MAX(started_at) FROM rounds WHERE workflow_id = ?1",
            [workflow_id],
            |r| r.get(0),
        )?;
        Ok(WorkflowFacts {
            question_at,
            summary_at,
            outcome,
            rounds: usize::try_from(rounds).unwrap_or(0),
            last_round_at,
        })
    }

    /// The Question node with this question id.
    pub fn question_node(&self, workflow_id: &str, question_id: &str) -> Result<Option<Node>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {NODE_COLS} FROM nodes WHERE workflow_id = ?1 AND kind = 'question' \
             AND json_extract(payload_json, '$.question_id') = ?2"
        ))?;
        let mut nodes = collect(
            stmt.query_map(params![workflow_id, question_id], node_row)?,
            finish_node,
        )?;
        Ok(nodes.pop())
    }

    // ---- raw events ----

    pub fn insert_raw_event(&self, e: &NewRawEvent<'_>) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO raw_events (workflow_id, round_id, branch_id, seq, kind, json, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![e.workflow_id, e.round_id, e.branch_id, e.seq, e.kind, e.json, now_ms()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Raw events in `[first, last]`, optionally restricted to one lane
    /// (`Some(None)` = main lane, `Some(Some(id))` = that branch).
    /// The `[first, last]` raw event ids of a round, if it recorded any.
    pub fn round_raw_span(&self, round_id: &str) -> Result<Option<(i64, i64)>> {
        let span: (Option<i64>, Option<i64>) = self.conn.query_row(
            "SELECT MIN(id), MAX(id) FROM raw_events WHERE round_id = ?1",
            [round_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(span.0.zip(span.1))
    }

    pub fn raw_events(
        &self,
        workflow_id: &str,
        first: i64,
        last: i64,
        lane: Option<Option<&str>>,
    ) -> Result<Vec<RawEventRow>> {
        let base = "SELECT id, workflow_id, round_id, branch_id, seq, kind, json, created_at FROM raw_events \
                    WHERE workflow_id = ?1 AND id BETWEEN ?2 AND ?3";
        let map = |r: &Row<'_>| {
            Ok(RawEventRow {
                id: r.get(0)?,
                workflow_id: r.get(1)?,
                round_id: r.get(2)?,
                branch_id: r.get(3)?,
                seq: r.get(4)?,
                kind: r.get(5)?,
                json: r.get(6)?,
                created_at: r.get(7)?,
            })
        };
        let rows = match lane {
            None => {
                let mut stmt = self.conn.prepare(&format!("{base} ORDER BY id"))?;
                stmt.query_map(params![workflow_id, first, last], map)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            }
            Some(None) => {
                let mut stmt = self
                    .conn
                    .prepare(&format!("{base} AND branch_id IS NULL ORDER BY id"))?;
                stmt.query_map(params![workflow_id, first, last], map)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            }
            Some(Some(branch)) => {
                let mut stmt = self
                    .conn
                    .prepare(&format!("{base} AND branch_id = ?4 ORDER BY id"))?;
                stmt.query_map(params![workflow_id, first, last, branch], map)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            }
        };
        Ok(rows)
    }

    // ---- snapshots ----

    pub fn insert_snapshot(
        &self,
        workflow_id: &str,
        section_no: u32,
        kind: SnapshotKind,
        tree_sha: &str,
    ) -> Result<Snapshot> {
        let s = Snapshot {
            id: new_id(),
            workflow_id: workflow_id.to_owned(),
            section_no,
            kind,
            tree_sha: tree_sha.to_owned(),
            created_at: now_ms(),
        };
        self.conn.execute(
            "INSERT INTO snapshots (id, workflow_id, section_no, kind, tree_sha, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                s.id,
                s.workflow_id,
                s.section_no,
                s.kind.as_str(),
                s.tree_sha,
                s.created_at
            ],
        )?;
        Ok(s)
    }

    pub fn snapshots(&self, workflow_id: &str) -> Result<Vec<Snapshot>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, workflow_id, section_no, kind, tree_sha, created_at FROM snapshots \
             WHERE workflow_id = ?1 ORDER BY created_at, section_no",
        )?;
        collect(
            stmt.query_map([workflow_id], snapshot_row)?,
            finish_snapshot,
        )
    }

    // ---- review comments ----

    pub fn insert_review_comment(&self, c: &NewReviewComment) -> Result<ReviewCommentRow> {
        let row = ReviewCommentRow {
            id: new_id(),
            workflow_id: c.workflow_id.clone(),
            from_snapshot: c.from_snapshot.clone(),
            to_snapshot: c.to_snapshot.clone(),
            preset: c.preset,
            path: c.path.clone(),
            side: c.side,
            line: c.line,
            body: c.body.clone(),
            sent_in_node: None,
            posted_to_pr: false,
            created_at: now_ms(),
        };
        self.conn.execute(
            &format!(
                "INSERT INTO review_comments ({COMMENT_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"
            ),
            params![
                row.id,
                row.workflow_id,
                row.from_snapshot,
                row.to_snapshot,
                row.preset.as_str(),
                row.path,
                row.side.as_str(),
                row.line,
                row.body,
                row.sent_in_node,
                row.posted_to_pr,
                row.created_at
            ],
        )?;
        Ok(row)
    }

    pub fn review_comments(&self, workflow_id: &str) -> Result<Vec<ReviewCommentRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COMMENT_COLS} FROM review_comments WHERE workflow_id = ?1 ORDER BY created_at"
        ))?;
        collect(stmt.query_map([workflow_id], comment_row)?, finish_comment)
    }

    /// Comments not yet delivered to the agent.
    pub fn pending_review_comments(&self, workflow_id: &str) -> Result<Vec<ReviewCommentRow>> {
        Ok(self
            .review_comments(workflow_id)?
            .into_iter()
            .filter(|c| c.sent_in_node.is_none())
            .collect())
    }

    pub fn delete_review_comment(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM review_comments WHERE id = ?1 AND sent_in_node IS NULL",
            [id],
        )?;
        Ok(())
    }

    pub fn mark_comments_sent(&self, ids: &[String], node_id: &str) -> Result<()> {
        for id in ids {
            self.conn.execute(
                "UPDATE review_comments SET sent_in_node = ?2 WHERE id = ?1",
                params![id, node_id],
            )?;
        }
        Ok(())
    }

    pub fn mark_comments_posted(&self, ids: &[String]) -> Result<()> {
        for id in ids {
            self.conn.execute(
                "UPDATE review_comments SET posted_to_pr = 1 WHERE id = ?1",
                [id],
            )?;
        }
        Ok(())
    }

    // ---- GitHub entities ----

    pub fn upsert_gh_entity(
        &self,
        repo_id: &str,
        kind: &str,
        key: &str,
        json: &str,
    ) -> Result<GhEntityRow> {
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO gh_entities (id, repo_id, kind, number_or_sha, json, synced_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(repo_id, kind, number_or_sha) DO UPDATE SET json = excluded.json, synced_at = excluded.synced_at",
            params![new_id(), repo_id, kind, key, json, now],
        )?;
        self.gh_entity(repo_id, kind, key)?
            .ok_or_else(|| not_found("gh entity", key))
    }

    pub fn gh_entity(&self, repo_id: &str, kind: &str, key: &str) -> Result<Option<GhEntityRow>> {
        self.conn
            .query_row(
                "SELECT id, repo_id, kind, number_or_sha, json, synced_at FROM gh_entities \
                 WHERE repo_id = ?1 AND kind = ?2 AND number_or_sha = ?3",
                params![repo_id, kind, key],
                |r| {
                    Ok(GhEntityRow {
                        id: r.get(0)?,
                        repo_id: r.get(1)?,
                        kind: r.get(2)?,
                        number_or_sha: r.get(3)?,
                        json: r.get(4)?,
                        synced_at: r.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn gh_entities(&self, repo_id: &str, kind: &str) -> Result<Vec<GhEntityRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_id, kind, number_or_sha, json, synced_at FROM gh_entities \
             WHERE repo_id = ?1 AND kind = ?2 ORDER BY synced_at DESC",
        )?;
        let rows = stmt
            .query_map(params![repo_id, kind], |r| {
                Ok(GhEntityRow {
                    id: r.get(0)?,
                    repo_id: r.get(1)?,
                    kind: r.get(2)?,
                    number_or_sha: r.get(3)?,
                    json: r.get(4)?,
                    synced_at: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn link_workflow(&self, workflow_id: &str, entity_id: &str, role: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO workflow_links (workflow_id, entity_id, role) VALUES (?1, ?2, ?3)",
            params![workflow_id, entity_id, role],
        )?;
        Ok(())
    }

    /// `(role, entity)` pairs linked to a workflow.
    pub fn workflow_links(&self, workflow_id: &str) -> Result<Vec<(String, GhEntityRow)>> {
        let mut stmt = self.conn.prepare(
            "SELECT l.role, e.id, e.repo_id, e.kind, e.number_or_sha, e.json, e.synced_at \
             FROM workflow_links l JOIN gh_entities e ON e.id = l.entity_id WHERE l.workflow_id = ?1",
        )?;
        let rows = stmt
            .query_map([workflow_id], |r| {
                Ok((
                    r.get(0)?,
                    GhEntityRow {
                        id: r.get(1)?,
                        repo_id: r.get(2)?,
                        kind: r.get(3)?,
                        number_or_sha: r.get(4)?,
                        json: r.get(5)?,
                        synced_at: r.get(6)?,
                    },
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ---- connectors & settings ----

    pub fn upsert_connector(&self, name: &str, path: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO connectors (name, path, enabled) VALUES (?1, ?2, 1) \
             ON CONFLICT(name) DO UPDATE SET path = excluded.path",
            params![name, path],
        )?;
        Ok(())
    }

    pub fn connectors(&self) -> Result<Vec<ConnectorRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, path, enabled FROM connectors ORDER BY name")?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ConnectorRow {
                    name: r.get(0)?,
                    path: r.get(1)?,
                    enabled: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn set_connector_enabled(&self, name: &str, enabled: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE connectors SET enabled = ?2 WHERE name = ?1",
            params![name, enabled],
        )?;
        Ok(())
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()
            .map_err(Into::into)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    // ---- read models ----

    pub fn workflow_view(&self, workflow_id: &str) -> Result<WorkflowView> {
        Ok(WorkflowView {
            workflow: self.workflow(workflow_id)?,
            nodes: self.nodes(workflow_id)?,
            branches: self.branches(workflow_id)?,
            rounds: self.rounds(workflow_id)?,
            snapshots: self.snapshots(workflow_id)?,
        })
    }
}
