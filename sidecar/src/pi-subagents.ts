// Subagents for Pi (ADR 0034). Pi has none of its own: Berth's `agent` tool
// runs a child Pi session in this process, in the same checkout, on the same
// provider and model. It reaches the app as a Claude Code subagent does
// (§5.4): the call opens a branch, the child's messages are that branch's
// raw events, and the call's end collects it. Settings' Subagents at once
// holds a call back past the limit (ADR 0032); Stop on a subagent aborts its
// session alone (ADR 0022). One in the background (started so, or moved there
// with Background) returns its call at once, collects when it finishes, and
// reports to the main agent with a message (ADR 0035).

import { randomUUID } from "node:crypto";
import { isAbsolute, resolve } from "node:path";

import type { AgentSession, AgentSessionEvent } from "@mariozechner/pi-coding-agent";

import { BranchTracker, textOf, type Branch } from "./branches.js";
import type { Emitter } from "./ndjson.js";
import { lastText, messageEvent, PI_AGENT_TOOL, type PiContent, type PiMessage } from "./pi-events.js";
import { subagentLimitText } from "./prompt.js";
import type { BranchStatus, JsonValue, SessionTask } from "./protocol.js";

export { PI_AGENT_TOOL };

export const AGENT_TOOL_DESCRIPTION =
  "Hand a self-contained task to a subagent: a fresh agent with its own context window, working in this checkout with read, bash, edit and write. It sees none of this conversation, so the prompt must say everything it needs. It reports back with its final message, which is this tool's result. Several agent calls in one message run at the same time. With run_in_background, the call returns at once and the report comes to you as a message when the subagent finishes.";

/** The `agent` tool's input, as JSON Schema (Pi validates it). */
export const AGENT_TOOL_SCHEMA = {
  type: "object",
  properties: {
    description: { type: "string", description: "What it does, in 3 to 5 words" },
    prompt: {
      type: "string",
      description: "The whole task: what to do, where, what to report back",
    },
    run_in_background: {
      type: "boolean",
      description:
        "Carry on while it works: this call returns at once, and its report comes to you as a message when it finishes",
    },
  },
  required: ["description", "prompt"],
  additionalProperties: false,
} as const;

export interface AgentInput {
  description: string;
  prompt: string;
  run_in_background?: boolean;
}

/** What the main agent reads when the user stops a subagent. */
export const STOPPED_TEXT = "The user stopped this subagent before it finished; carry on without its result.";
const INTERRUPTED_TEXT = "Interrupted by the user.";
const ENDED_TEXT = "The session ended.";
/** The call's result for one that goes on in the background. */
export const STARTED_TEXT =
  "Started in the background. Its report will come to you as a message when it finishes: carry on with other work meanwhile, and don't wait or poll for it.";
export const MOVED_TEXT =
  "The user moved this subagent to the background. Its report will come to you as a message when it finishes: carry on with other work meanwhile.";
/** How long a call waits for its branch to open from the stream. */
const SPAWN_WAIT_MS = 2_000;
/** How long a finished subagent's last events may trail its run: Pi hands
 * them to listeners from a queue of its own. */
const SETTLE_WAIT_MS = 1_000;

interface Running {
  taskId: string;
  toolCallId: string;
  /** Once the stream opened it. */
  branch?: Branch;
  description: string;
  startedAt: number;
  child?: AgentSession;
  /** Why it ends early: the user stopped it, interrupted the round, or the
   * session ended. */
  cancelled?: string;
  costUsd: number;
  /** Its call returned: it collects when it finishes, and reports (ADR 0035). */
  background: boolean;
  /** Returns a foreground one's call now (Background). */
  release?: () => void;
  /** Stops following the main run's abort. */
  detach?: () => void;
}

/** How a subagent ended. */
export interface End {
  status: BranchStatus;
  text: string;
}

interface Outcome extends End {
  costUsd: number;
  /** Held back by the limit: it never ran. */
  heldBack?: boolean;
}

/** A background subagent finished: what the main agent hears of it. */
export interface Report extends End {
  taskId: string;
  toolCallId: string;
  description: string;
  /** The user stopped it: nothing to start a round for. */
  stopped: boolean;
}

export interface PiSubagentsDeps {
  emitter: Emitter;
  now: () => number;
  /** The checkout, where relative paths point. */
  cwd: () => string;
  /** The main session's id, which a subagent's events carry. */
  sessionId: () => string | undefined;
  /** A raw event, on `branchId`'s lane. */
  raw(kind: string, json: { [key: string]: JsonValue }, branchId: string): void;
  /** A subagent's model calls cost this (the session's total counts it). */
  spent(usd: number): void;
  /** A background subagent ended (its branch collected). */
  finished(report: Report): void;
}

export class PiSubagents {
  private readonly branches: BranchTracker;
  /** By the `agent` call that started it. */
  private readonly running = new Map<string, Running>();
  private readonly outcomes = new Map<string, Outcome>();
  /** A subagent's own tool calls: whose lane `record_decision` is on. */
  private readonly calls = new Map<string, string>();
  private readonly waiting = new Map<string, Array<(b: Branch) => void>>();

  constructor(
    private readonly deps: PiSubagentsDeps,
    /** Settings' Subagents at once; undefined is no limit of Berth's. */
    private readonly max: number | undefined,
  ) {
    this.branches = new BranchTracker(deps.now);
  }

  /** The main agent's message is out (its raw event sent): the subagents it
   * starts open their branches, one parallel group per message. */
  spawned(messageId: string, content: readonly PiContent[]): void {
    for (const c of content) {
      if (c.type !== "toolCall" || c.name !== PI_AGENT_TOOL || this.branches.get(c.id)) continue;
      this.open(c.id, c.arguments, messageId);
    }
  }

  private open(toolCallId: string, input: unknown, parallelGroup?: string): Branch {
    const spawned = this.branches.spawn({ toolUseId: toolCallId, input, parallelGroup });
    this.deps.emitter.emit({ type: "branch.spawned", ...spawned });
    const branch = this.branches.get(toolCallId) as Branch;
    for (const w of this.waiting.get(toolCallId) ?? []) w(branch);
    this.waiting.delete(toolCallId);
    return branch;
  }

  /** The branch a call opened; the stream may bring it a moment after the
   * call starts running. */
  private branchOf(toolCallId: string, input: AgentInput): Promise<Branch> {
    const known = this.branches.get(toolCallId);
    if (known) return Promise.resolve(known);
    return new Promise((done) => {
      const timer = setTimeout(() => {
        this.waiting.delete(toolCallId);
        done(this.branches.get(toolCallId) ?? this.open(toolCallId, input));
      }, SPAWN_WAIT_MS);
      const list = this.waiting.get(toolCallId) ?? [];
      list.push((b) => {
        clearTimeout(timer);
        done(b);
      });
      this.waiting.set(toolCallId, list);
    });
  }

  /** The `agent` tool: a subagent's call, until its report, or until it goes
   * on in the background. What it throws, the main agent reads as the
   * tool's error. */
  async run(
    toolCallId: string,
    input: AgentInput,
    signal: AbortSignal | undefined,
    openChild: (branchId: string) => Promise<AgentSession>,
  ): Promise<string> {
    // Before any await: calls from one message start together. One in the
    // background counts until it finishes.
    if (this.max !== undefined && this.running.size >= this.max) {
      const why = subagentLimitText(this.max);
      this.outcomes.set(toolCallId, { status: "cancelled", text: why, costUsd: 0, heldBack: true });
      throw new Error(why);
    }
    const r: Running = {
      taskId: randomUUID(),
      toolCallId,
      description: input.description,
      startedAt: this.deps.now(),
      costUsd: 0,
      background: input.run_in_background === true,
    };
    this.running.set(toolCallId, r);
    // Interrupt ends the main run, and a subagent its call waits on; one in
    // the background goes on until stopped.
    if (!r.background && signal) {
      const abort = () => this.cancel(r, INTERRUPTED_TEXT);
      signal.addEventListener("abort", abort, { once: true });
      r.detach = () => signal.removeEventListener("abort", abort);
      if (signal.aborted) abort();
    }
    const life = this.live(r, input, openChild).then((end) => this.finish(r, end));
    if (r.background) return STARTED_TEXT;
    const moved = new Promise<"moved">((done) => (r.release = () => done("moved")));
    const first = await Promise.race([life, moved]);
    if (first === "moved") return MOVED_TEXT;
    if (first.status !== "ok") throw new Error(first.text);
    return first.text;
  }

  /** A subagent from its branch to its last message. Never throws. */
  private async live(
    r: Running,
    input: AgentInput,
    openChild: (branchId: string) => Promise<AgentSession>,
  ): Promise<End> {
    try {
      const branch = await this.branchOf(r.toolCallId, input);
      r.branch = branch;
      this.emitTasks();
      if (r.cancelled) return { status: "cancelled", text: r.cancelled };
      const child = await openChild(branch.branchId);
      r.child = child;
      let settled!: () => void;
      const idle = new Promise<void>((done) => (settled = done));
      child.subscribe((e) => {
        this.onChildEvent(r, branch, e);
        if (e.type === "agent_end") settled();
      });
      if (r.cancelled) return { status: "cancelled", text: r.cancelled };
      try {
        await child.prompt(input.prompt);
        // Its last messages reach the app before its branch collects.
        await within(idle, SETTLE_WAIT_MS);
      } catch (e) {
        if (!r.cancelled) return { status: "error", text: message(e) };
      }
      if (r.cancelled) return { status: "cancelled", text: r.cancelled };
      const last = [...child.messages].reverse().find((m) => m.role === "assistant") as
        | Extract<PiMessage, { role: "assistant" }>
        | undefined;
      if (last?.stopReason === "error") {
        return { status: "error", text: last.errorMessage ?? "The model's request failed." };
      }
      if (last?.stopReason === "aborted") return { status: "cancelled", text: INTERRUPTED_TEXT };
      const report = last ? lastText(last) : undefined;
      return { status: "ok", text: report ?? "(The subagent finished without a report.)" };
    } catch (e) {
      return { status: "error", text: message(e) };
    }
  }

  /** It ended. A foreground one's branch collects when its call ends
   * ({@link ended}); one in the background collects now and reports. */
  private finish(r: Running, end: End): End {
    r.detach?.();
    this.running.delete(r.toolCallId);
    for (const [call, branchId] of this.calls) if (branchId === r.branch?.branchId) this.calls.delete(call);
    r.child?.dispose();
    this.emitTasks();
    if (!r.background) {
      this.outcomes.set(r.toolCallId, { ...end, costUsd: r.costUsd });
      return end;
    }
    const branch = r.branch;
    if (branch && !branch.collected) {
      this.deps.emitter.emit({
        type: "branch.collected",
        ...this.branches.collect(branch, { resultText: end.text, status: end.status }),
        ...(r.costUsd > 0 ? { cost_usd: r.costUsd } : {}),
      });
    }
    if (r.cancelled !== ENDED_TEXT) {
      this.deps.finished({
        ...end,
        taskId: r.taskId,
        toolCallId: r.toolCallId,
        description: r.description,
        stopped: r.cancelled === STOPPED_TEXT,
      });
    }
    return end;
  }

  /** The main session finished an `agent` call (Pi's `tool_execution_end`):
   * its branch collects, as soon as it ends rather than with the others
   * started beside it; one that goes on in the background collects later. */
  ended(toolCallId: string, result: unknown, isError: boolean): void {
    const branch = this.branches.get(toolCallId);
    if (!branch || branch.collected) return;
    if (this.running.get(toolCallId)?.background) return;
    const o = this.outcomes.get(toolCallId);
    this.outcomes.delete(toolCallId);
    const content = typeof result === "object" && result !== null ? (result as { content?: unknown }).content : "";
    const collected = this.branches.collect(branch, {
      resultText: o?.text ?? textOf(content),
      status: o?.status ?? (isError ? "error" : "ok"),
      ...(o?.heldBack ? { durationMs: 0, toolCalls: 0 } : {}),
    });
    this.deps.emitter.emit({
      type: "branch.collected",
      ...collected,
      ...(o && o.costUsd > 0 ? { cost_usd: o.costUsd } : {}),
    });
  }

  /** Background (`session.background`): subagents the main agent waits on go
   * on in the background, their calls returned; `toolUseId`, that one only.
   * How many moved. */
  toBackground(toolUseId?: string): number {
    let moved = 0;
    for (const r of this.running.values()) {
      if (r.background || !r.release || (toolUseId && r.toolCallId !== toolUseId)) continue;
      r.background = true;
      r.detach?.();
      r.release();
      moved += 1;
    }
    if (moved > 0) this.emitTasks();
    return moved;
  }

  /** Stop one subagent (`task.stop`); the round goes on without it. */
  stop(taskId: string): boolean {
    const r = [...this.running.values()].find((x) => x.taskId === taskId);
    if (!r) return false;
    this.cancel(r, STOPPED_TEXT);
    return true;
  }

  /** The session ends: every subagent with it, none reporting. */
  stopAll(): void {
    for (const r of this.running.values()) this.cancel(r, ENDED_TEXT);
  }

  private cancel(r: Running, why: string): void {
    r.cancelled ??= why;
    void r.child?.abort().catch(() => undefined);
  }

  /** Whose lane a tool call is on: a subagent's, or undefined for the main agent's. */
  laneOf(toolCallId: string | undefined): string | undefined {
    return toolCallId ? this.calls.get(toolCallId) : undefined;
  }

  /** A subagent's `record_decision` call is about to run: it is on that branch. */
  bind(toolCallId: string, branchId: string): void {
    this.calls.set(toolCallId, branchId);
  }

  private onChildEvent(r: Running, branch: Branch, e: AgentSessionEvent): void {
    if (e.type !== "message_end") return;
    const m = e.message as PiMessage;
    if (m.role === "assistant" && "content" in m) {
      const a = m as Extract<PiMessage, { role: "assistant" }>;
      const usd = a.usage?.cost?.total ?? 0;
      r.costUsd += usd;
      if (usd > 0) this.deps.spent(usd);
      const before = branch.toolCalls;
      for (const c of a.content) {
        if (c.type !== "toolCall") continue;
        const path = c.arguments.path;
        const input =
          typeof path === "string" && path && !isAbsolute(path)
            ? { ...c.arguments, path: resolve(this.deps.cwd(), path) }
            : c.arguments;
        this.branches.recordToolUse(branch, c.name, input);
      }
      if (branch.toolCalls !== before) this.emitTasks();
    }
    const event = messageEvent(m, this.deps.sessionId(), undefined, r.toolCallId);
    if (event) this.deps.raw(event.kind, event.json, branch.branchId);
  }

  /** The running subagents, as the Tasks panel lists them (ADR 0022). */
  private emitTasks(): void {
    const tasks: SessionTask[] = [...this.running.values()]
      .sort((a, b) => a.startedAt - b.startedAt)
      .flatMap((r) =>
        r.branch
          ? [
              {
                task_id: r.taskId,
                kind: "local_agent",
                description: r.description,
                background: r.background,
                started_at: r.startedAt,
                tool_uses: r.branch.toolCalls,
                tool_use_id: r.toolCallId,
              },
            ]
          : [],
      );
    this.deps.emitter.emit({ type: "session.tasks", tasks });
  }
}

/** `p`, or nothing after `ms`. */
async function within(p: Promise<void>, ms: number): Promise<void> {
  let timer: NodeJS.Timeout | undefined;
  await Promise.race([p, new Promise<void>((done) => (timer = setTimeout(done, ms)))]);
  clearTimeout(timer);
}

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}
