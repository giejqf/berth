// The session's live tasks (ADR 0022): every subagent, shell command and
// monitor Claude Code runs for it, foreground or background, from its
// task_started until its task_notification. Housekeeping tasks stay out.

import type { SessionTask } from "./protocol.js";

export type TaskStarted = {
  task_id: string;
  task_type?: string;
  description: string;
  subagent_type?: string;
  is_backgrounded?: boolean;
  tool_use_id?: string;
  ambient?: boolean;
  skip_transcript?: boolean;
};

export type TaskProgress = {
  task_id: string;
  description?: string;
  summary?: string;
  usage?: { tool_uses: number };
};

export type TaskUpdated = {
  task_id: string;
  patch: { status?: string; description?: string; is_backgrounded?: boolean };
};

export type BackgroundTask = {
  task_id: string;
  task_type: string;
  description: string;
  subagent_type?: string;
  ambient?: boolean;
};

/** Statuses a task_updated patch ends a task with. */
const ENDED = new Set(["completed", "failed", "killed"]);

export class TaskTracker {
  private readonly tasks = new Map<string, SessionTask>();

  constructor(private readonly now: () => number) {}

  /** Each method returns whether the list changed. */
  started(t: TaskStarted): boolean {
    if (t.ambient || t.skip_transcript) return false;
    this.tasks.set(t.task_id, {
      task_id: t.task_id,
      kind: t.task_type ?? (t.subagent_type ? "local_agent" : "task"),
      description: t.description,
      ...agent(t.subagent_type),
      background: t.is_backgrounded ?? false,
      started_at: this.tasks.get(t.task_id)?.started_at ?? this.now(),
      ...(t.tool_use_id ? { tool_use_id: t.tool_use_id } : {}),
    });
    return true;
  }

  progress(p: TaskProgress): boolean {
    const task = this.tasks.get(p.task_id);
    if (!task) return false;
    const summary = p.summary?.trim();
    const next: SessionTask = {
      ...task,
      ...(p.description ? { description: p.description } : {}),
      ...(summary ? { summary } : {}),
      ...(p.usage ? { tool_uses: p.usage.tool_uses } : {}),
    };
    return this.replace(task, next);
  }

  updated(u: TaskUpdated): boolean {
    const task = this.tasks.get(u.task_id);
    if (!task) return false;
    if (u.patch.status && ENDED.has(u.patch.status)) return this.ended(u.task_id);
    const next: SessionTask = {
      ...task,
      ...(u.patch.description ? { description: u.patch.description } : {}),
      ...(u.patch.is_backgrounded !== undefined ? { background: u.patch.is_backgrounded } : {}),
    };
    return this.replace(task, next);
  }

  ended(taskId: string): boolean {
    return this.tasks.delete(taskId);
  }

  /** `background_tasks_changed`: every live background task, so a background
   * task it leaves out has ended. */
  background(list: readonly BackgroundTask[]): boolean {
    const live = list.filter((t) => !t.ambient);
    const ids = new Set(live.map((t) => t.task_id));
    let changed = false;
    for (const [id, task] of this.tasks) {
      if (task.background && !ids.has(id)) changed = this.tasks.delete(id) || changed;
    }
    for (const t of live) {
      const known = this.tasks.get(t.task_id);
      const next: SessionTask = {
        ...(known ?? { task_id: t.task_id, started_at: this.now() }),
        kind: t.task_type,
        description: t.description,
        ...agent(t.subagent_type),
        background: true,
      };
      changed = (known ? this.replace(known, next) : (this.tasks.set(t.task_id, next), true)) || changed;
    }
    return changed;
  }

  /** Oldest first. */
  list(): SessionTask[] {
    return [...this.tasks.values()].sort((a, b) => a.started_at - b.started_at);
  }

  private replace(task: SessionTask, next: SessionTask): boolean {
    if (JSON.stringify(task) === JSON.stringify(next)) return false;
    this.tasks.set(task.task_id, next);
    return true;
  }
}

/** A subagent's type; "main-session" is a backgrounded main session, not one. */
function agent(type: string | undefined): { agent?: string } {
  return type && type !== "main-session" ? { agent: type } : {};
}
