// Agent SDK session lifecycle, round tracking, and nudges (AGENTS.md §5.3, §10).
//
// The sidecar is stateless across restarts: everything it learns is emitted to
// the app, which persists it. It owns only what must happen in-process: blocking
// tool calls, branch attribution from the stream, and the nudge loop.

import { randomUUID } from "node:crypto";
import { existsSync, readFileSync, statSync } from "node:fs";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  query as sdkQuery,
  type ModelInfo,
  type Options,
  type SDKMessage,
  type SDKUserMessage,
} from "@anthropic-ai/claude-agent-sdk";

import { AGENT_TOOLS, BranchTracker, agentResult, taskStatus, type Branch } from "./branches.js";
import type { Logger } from "./log.js";
import type { Emitter } from "./ndjson.js";
import type {
  AskUserInput,
  Effort,
  FollowUp,
  FollowUpInput,
  JsonValue,
  PresentArtifactInput,
  RecordDecisionInput,
  RecordOutcomeInput,
  RequestSlotInput,
  RequestSlotResult,
  SessionResume,
  SessionStart,
  TerminalKind,
  TurnAttachment,
  WriteSummaryInput,
} from "./protocol.js";
import { AsyncQueue } from "./queue.js";
import { berthToolName, createBerthServer, type CallContext, type ToolHost, type ToolOutcome } from "./tools.js";

export const BERTH_SYSTEM_PROMPT = `You are working inside Berth. The user never reads or edits code: they follow your decisions, answer your questions, review diffs, and ship pull requests.

How to work:
- Announce each meaningful decision with the berth \`record_decision\` tool the moment you make it (an approach chosen, a trade-off accepted, a convention adopted). Short title; the why goes in the rationale.
- Never narrate file edits or tool use in prose. The user reviews the diff separately.
- End every round with exactly one of these berth tools: \`ask_user\` when you need the user's input, \`write_summary\` when you finished a chunk of work for the user to review, or \`record_outcome\` when the workflow is done (usually right after opening a PR). Never end a turn without one of them, and never call two.
- Ask questions only through \`ask_user\`, never through AskUserQuestion or plain text.
- Use \`gh\` for GitHub. Open PRs with \`gh pr create\`, then call \`record_outcome\` with kind "pr" and the PR URL as ref.
- Use the connector CLIs described by their skills for external systems (Linear, PostHog, ...).
- Run the repository's gates (tests, lint, typecheck) before \`write_summary\` when you changed code.
- When you make something the user should see (a screenshot, a screen recording, a rendered chart or page, a report), save it to a file and call \`present_artifact\` with its path and a short title. Never paste images or long output into text.
- Files the user attached are listed at the end of their message, with paths you can read; attached images are also shown to you directly.
- Subagents may only call \`record_decision\`; they report everything else back to you.`;

export const NUDGE_TEXT = "End this round with ask_user, write_summary, or record_outcome.";
export const MAX_NUDGES = 2;
export const SUBAGENT_TOOL_ERROR = "Not available to subagents; report this back to the main agent.";
/** Largest attached image sent to Claude as an image block (larger ones go by path only). */
const MAX_IMAGE_BYTES = 5 * 1024 * 1024;
const IMAGE_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp"] as const;
type ImageType = (typeof IMAGE_TYPES)[number];
/** How long a tool handler waits for its tool_use to appear in the stream. */
const ATTRIBUTION_WAIT_MS = 3000;

/** The subset of the SDK's `Query` the session uses (lets tests supply a fake). */
export interface AgentQuery extends AsyncIterable<SDKMessage> {
  interrupt(): Promise<unknown>;
  close(): void;
  setModel(model?: string): Promise<void>;
  applyFlagSettings(settings: { effortLevel?: Effort | null }): Promise<void>;
  supportedModels(): Promise<ModelInfo[]>;
}

const EFFORTS: readonly Effort[] = ["low", "medium", "high", "xhigh", "max"];

export type QueryFn = (params: { prompt: AsyncIterable<SDKUserMessage>; options: Options }) => AgentQuery;

export const defaultQuery: QueryFn = (params) => sdkQuery(params);

/** `BERTH_CLAUDE_PATH`, else a `claude` shipped next to the bundle, else the SDK's own binary. */
export function claudePath(): string | undefined {
  if (process.env.BERTH_CLAUDE_PATH) return process.env.BERTH_CLAUDE_PATH;
  const bundled = join(dirname(fileURLToPath(import.meta.url)), "claude");
  return existsSync(bundled) ? bundled : undefined;
}

type TerminalTool = "ask_user" | "write_summary" | "record_outcome";

interface Round {
  startedAt: number;
  startTotalUsd: number;
  nudges: number;
  /** Accepted terminal call; `ask_user` clears again once answered (§5.3). */
  terminal: TerminalTool | null;
  interrupted: boolean;
  lastText?: string;
}

interface ObservedToolUse {
  id: string;
  name: string;
  input: unknown;
  parentToolUseId: string | null;
  claimed: boolean;
}

interface Pending<T> {
  resolve(value: T): void;
  reject(err: Error): void;
}

export interface SessionDeps {
  emitter: Emitter;
  log: Logger;
  query?: QueryFn;
  now?: () => number;
}

export class Session implements ToolHost {
  private readonly emitter: Emitter;
  private readonly log: Logger;
  private readonly queryFn: QueryFn;
  private readonly now: () => number;
  private readonly input = new AsyncQueue<SDKUserMessage>();
  private readonly branches: BranchTracker;
  private readonly toolUses = new Map<string, ObservedToolUse>();
  private readonly toolUseWaiters = new Map<string, Array<(t: ObservedToolUse) => void>>();
  private readonly questions = new Map<string, Pending<string>>();
  private readonly slotRequests = new Map<string, Pending<RequestSlotResult>>();

  private q?: AgentQuery;
  private consuming?: Promise<void>;
  private startReplyTo?: string;
  private claudeSessionId?: string;
  private round?: Round;
  private cwd = "";
  private seq = 0;
  private totalUsd = 0;

  constructor(deps: SessionDeps) {
    this.emitter = deps.emitter;
    this.log = deps.log;
    this.queryFn = deps.query ?? defaultQuery;
    this.now = deps.now ?? Date.now;
    this.branches = new BranchTracker(this.now);
  }

  get started(): boolean {
    return this.q !== undefined;
  }

  /** Resolves when the SDK stream ends. */
  get done(): Promise<void> {
    return this.consuming ?? Promise.resolve();
  }

  // ---- app -> sidecar ----

  start(msg: SessionStart | SessionResume, replyTo: string): void {
    if (this.q) {
      this.error("session already started", false, replyTo);
      return;
    }
    this.startReplyTo = replyTo;
    this.cwd = msg.cwd;
    const resume = "claude_session_id" in msg ? msg.claude_session_id : undefined;
    if (resume) this.claudeSessionId = resume;
    this.q = this.queryFn({ prompt: this.input, options: this.options(msg, resume) });
    this.consuming = this.consume(this.q);
    void this.reportModels(this.q);
  }

  /** Change the model and effort from the agent's next request on, mid-round
   * included; `null` goes back to the default. */
  async configure(model: string | null, effort: Effort | null): Promise<void> {
    if (!this.q) {
      this.error("session.configure before session.start", false);
      return;
    }
    try {
      await this.q.setModel(model ?? undefined);
      await this.q.applyFlagSettings({ effortLevel: effort });
    } catch (e) {
      this.error(`Could not switch the model: ${e instanceof Error ? e.message : String(e)}`, false);
    }
  }

  /** Tell the app which models (and efforts) it can offer. */
  private async reportModels(q: AgentQuery): Promise<void> {
    try {
      const models = (await q.supportedModels()).map((m) => ({
        value: m.value,
        display_name: m.displayName,
        description: m.description,
        efforts: (m.supportedEffortLevels ?? []).filter((e): e is Effort => EFFORTS.includes(e as Effort)),
      }));
      if (models.length > 0) this.emitter.emit({ type: "session.models", models });
    } catch (e) {
      this.log.warn({ err: e }, "supportedModels failed");
    }
  }

  turn(text: string, attachments: TurnAttachment[] = []): void {
    if (!this.q) {
      this.error("session.turn before session.start", false);
      return;
    }
    // A turn sent while a round runs is folded into it by the SDK.
    this.round ??= this.newRound();
    this.push(text, attachments);
  }

  async interrupt(): Promise<void> {
    if (!this.q || !this.round) return;
    this.round.interrupted = true;
    for (const [id, p] of this.questions) {
      p.reject(new Error("Interrupted by the user."));
      this.questions.delete(id);
    }
    for (const [id, p] of this.slotRequests) {
      p.resolve({ approved: false });
      this.slotRequests.delete(id);
    }
    try {
      await this.q.interrupt();
    } catch (e) {
      this.log.warn({ err: e }, "interrupt failed");
    }
  }

  stop(): void {
    this.input.end();
    this.q?.close();
  }

  answer(questionId: string, text: string): void {
    const pending = this.questions.get(questionId);
    if (!pending) {
      this.error(`no pending question ${questionId}`, false);
      return;
    }
    this.questions.delete(questionId);
    pending.resolve(text);
  }

  resolveSlotRequest(requestId: string, approved: boolean, slotId?: string): void {
    const pending = this.slotRequests.get(requestId);
    if (!pending) {
      this.error(`no pending slot request ${requestId}`, false);
      return;
    }
    this.slotRequests.delete(requestId);
    pending.resolve(slotId ? { approved, slot_id: slotId } : { approved });
  }

  // ---- SDK stream ----

  private options(msg: SessionStart, resume: string | undefined): Options {
    const skills =
      msg.connector_skills.length > 0
        ? `\n\nConnector skills installed in this checkout:\n${msg.connector_skills
            .map((s) => `- ${s.name}: ${s.description}`)
            .join("\n")}`
        : "";
    const append = [BERTH_SYSTEM_PROMPT + skills, msg.system_prompt].filter((s) => s.trim().length > 0).join("\n\n");
    return {
      cwd: msg.cwd,
      systemPrompt: { type: "preset", preset: "claude_code", append, snapshot: true },
      // Always auto-approve (§1.6): isolation is the slot's job.
      permissionMode: "bypassPermissions",
      allowDangerouslySkipPermissions: true,
      mcpServers: { berth: createBerthServer(this) },
      allowedTools: ["mcp__berth__*", ...msg.allowed_tools],
      disallowedTools: ["AskUserQuestion"],
      skills: "all",
      env: { ...process.env, ...msg.env, CLAUDE_AGENT_SDK_CLIENT_APP: "berth/0.1.0" },
      ...(resume ? { resume } : {}),
      ...(msg.model ? { model: msg.model } : {}),
      ...(msg.effort ? { effort: msg.effort } : {}),
      ...(claudePath() ? { pathToClaudeCodeExecutable: claudePath() } : {}),
      stderr: (data: string) => this.log.debug({ claude: data.trimEnd() }, "claude stderr"),
    };
  }

  private async consume(q: AgentQuery): Promise<void> {
    try {
      for await (const msg of q) this.handle(msg);
    } catch (e) {
      this.error(`agent stream failed: ${(e as Error).message}`, true);
    } finally {
      for (const p of this.questions.values()) p.reject(new Error("Session ended."));
      this.questions.clear();
    }
  }

  /** Exposed for tests; normally driven by {@link consume}. */
  handle(msg: SDKMessage): void {
    const parent = "parent_tool_use_id" in msg ? (msg.parent_tool_use_id ?? null) : null;
    const branch = parent ? this.branches.get(parent) : undefined;
    this.emitter.emit({
      type: "raw.event",
      seq: ++this.seq,
      kind: msg.type,
      json: msg as unknown as JsonValue,
      ...(branch ? { branch_id: branch.branchId } : {}),
    });

    switch (msg.type) {
      case "system":
        if (msg.subtype === "init") this.onInit(msg.session_id);
        else if (msg.subtype === "task_notification") this.onTaskNotification(msg);
        break;
      case "assistant":
        this.onAssistant(msg.message.id, msg.message.content, parent, branch);
        break;
      case "user":
        this.onUser(msg.message.content, msg.tool_use_result);
        break;
      case "result":
        this.onResult(msg.total_cost_usd, msg.subtype !== "success" || msg.is_error);
        break;
      default:
        break;
    }
  }

  private onInit(sessionId: string): void {
    // Reply once to session.start/resume, and again only if the id changes.
    if (this.startReplyTo === undefined && sessionId === this.claudeSessionId) return;
    this.claudeSessionId = sessionId;
    this.emitter.emit({ type: "session.started", claude_session_id: sessionId }, this.startReplyTo);
    this.startReplyTo = undefined;
  }

  private onAssistant(
    messageId: string,
    content: ReadonlyArray<{ type: string }>,
    parent: string | null,
    branch: Branch | undefined,
  ): void {
    // The CLI may start a turn by itself (e.g. a background agent finished).
    if (!parent) this.round ??= this.newRound();
    for (const block of content) {
      if (block.type === "text" && !parent && this.round) {
        const text = (block as { text?: unknown }).text;
        if (typeof text === "string" && text.trim()) this.round.lastText = text;
      }
      if (block.type !== "tool_use") continue;
      const b = block as unknown as { id: string; name: string; input: unknown };
      this.observeToolUse({ id: b.id, name: b.name, input: b.input, parentToolUseId: parent, claimed: false });
      if (branch) this.branches.recordToolUse(branch, b.name, b.input);
      if (AGENT_TOOLS.has(b.name)) {
        const spawned = this.branches.spawn({
          toolUseId: b.id,
          input: b.input,
          parentBranchId: branch?.branchId,
          // Spawns from one assistant message share its id; a group of N > 1 is parallel.
          parallelGroup: messageId,
        });
        this.emitter.emit({ type: "branch.spawned", ...spawned });
      }
    }
  }

  private onUser(content: unknown, toolUseResult: unknown): void {
    if (!Array.isArray(content)) return;
    for (const raw of content) {
      const block = raw as { type?: unknown; tool_use_id?: unknown; content?: unknown; is_error?: boolean };
      if (block.type !== "tool_result" || typeof block.tool_use_id !== "string") continue;
      const branch = this.branches.get(block.tool_use_id);
      if (!branch || branch.collected) continue;
      const r = agentResult(block, toolUseResult);
      if (r.kind === "background") {
        branch.background = true;
        continue;
      }
      this.emitter.emit({
        type: "branch.collected",
        ...this.branches.collect(branch, {
          resultText: r.text,
          status: r.isError ? "error" : "ok",
          durationMs: r.durationMs,
          toolCalls: r.toolCalls,
        }),
      });
    }
  }

  private onTaskNotification(msg: {
    tool_use_id?: string;
    status: string;
    summary: string;
    usage?: { tool_uses: number; duration_ms: number };
  }): void {
    const branch = msg.tool_use_id ? this.branches.get(msg.tool_use_id) : undefined;
    if (!branch || branch.collected) return;
    this.emitter.emit({
      type: "branch.collected",
      ...this.branches.collect(branch, {
        resultText: msg.summary,
        status: taskStatus(msg.status),
        durationMs: msg.usage?.duration_ms,
        toolCalls: msg.usage ? Math.max(msg.usage.tool_uses, branch.toolCalls) : undefined,
      }),
    });
  }

  private onResult(totalUsd: number, isError: boolean): void {
    if (Number.isFinite(totalUsd)) this.totalUsd = totalUsd;
    this.emitter.emit({ type: "session.cost", total_usd: this.totalUsd });
    const round = this.round;
    if (!round) return;
    if (round.interrupted) return this.endRound("interrupted");
    if (round.terminal === "write_summary" || round.terminal === "record_outcome") {
      return this.endRound(round.terminal);
    }
    if (isError) return this.endRound("error");
    if (round.nudges < MAX_NUDGES) {
      round.nudges += 1;
      this.push(NUDGE_TEXT);
      return;
    }
    this.endRound("unterminated");
  }

  private endRound(kind: TerminalKind): void {
    const round = this.round;
    if (!round) return;
    this.round = undefined;
    this.emitter.emit({
      type: "round.ended",
      terminal_kind: kind,
      cost_usd: Math.max(0, this.totalUsd - round.startTotalUsd),
      duration_ms: Math.max(0, this.now() - round.startedAt),
      nudges: round.nudges,
      ...(round.lastText ? { last_text: round.lastText } : {}),
    });
  }

  private newRound(): Round {
    return { startedAt: this.now(), startTotalUsd: this.totalUsd, nudges: 0, terminal: null, interrupted: false };
  }

  private push(text: string, attachments: TurnAttachment[] = []): void {
    const images = attachments.flatMap((a) => this.imageBlock(a));
    this.input.push({
      type: "user",
      message: {
        role: "user",
        content: images.length > 0 ? [{ type: "text", text }, ...images] : text,
      },
      parent_tool_use_id: null,
      ...(this.claudeSessionId ? { session_id: this.claudeSessionId } : {}),
    });
  }

  /** An attached image as an image block; nothing if unreadable or too large. */
  private imageBlock(a: TurnAttachment) {
    const mime = IMAGE_TYPES.find((t) => t === a.mime);
    if (!mime) return [];
    try {
      if (statSync(a.path).size > MAX_IMAGE_BYTES) return [];
      const data = readFileSync(a.path).toString("base64");
      return [{ type: "image" as const, source: { type: "base64" as const, media_type: mime as ImageType, data } }];
    } catch (e) {
      this.log.warn({ err: e, path: a.path }, "attachment unreadable");
      return [];
    }
  }

  private error(message: string, fatal: boolean, replyTo?: string): void {
    this.log.error({ fatal }, message);
    this.emitter.emit({ type: "session.error", message, fatal }, replyTo);
  }

  // ---- attribution ----

  private observeToolUse(t: ObservedToolUse): void {
    this.toolUses.set(t.id, t);
    const waiters = this.toolUseWaiters.get(t.id);
    if (waiters) {
      this.toolUseWaiters.delete(t.id);
      for (const w of waiters) w(t);
    }
  }

  private waitForToolUse(id: string): Promise<ObservedToolUse | undefined> {
    const seen = this.toolUses.get(id);
    if (seen) return Promise.resolve(seen);
    return new Promise((resolve) => {
      const timer = setTimeout(() => resolve(undefined), ATTRIBUTION_WAIT_MS);
      const list = this.toolUseWaiters.get(id) ?? [];
      list.push((t) => {
        clearTimeout(timer);
        resolve(t);
      });
      this.toolUseWaiters.set(id, list);
    });
  }

  /** Which lane a tool call belongs to, from the stream (§5.4). */
  private async attribute(call: CallContext, input: unknown, tool: string): Promise<{ branchId?: string; inSubagent: boolean }> {
    let observed: ObservedToolUse | undefined;
    if (call.toolUseId) {
      observed = await this.waitForToolUse(call.toolUseId);
    } else {
      // Older CLIs: match the oldest unclaimed tool_use with the same name and input.
      const key = JSON.stringify(input);
      observed = [...this.toolUses.values()].find(
        (t) => !t.claimed && berthToolName(t.name) === tool && JSON.stringify(t.input) === key,
      );
    }
    if (observed) observed.claimed = true;
    const parent = observed?.parentToolUseId ?? null;
    if (!parent) return { inSubagent: false };
    const branch = this.branches.get(parent);
    return { branchId: branch?.branchId, inSubagent: true };
  }

  /** Accept a terminal call for the current round, or explain why not (§5.3). */
  private claimTerminal(tool: TerminalTool): string | undefined {
    this.round ??= this.newRound();
    const current = this.round.terminal;
    if (current) {
      return `This round already ended with ${current}; only the first terminal call counts. End your turn now.`;
    }
    this.round.terminal = tool;
    return undefined;
  }

  private followUps(input: FollowUpInput[] | undefined): FollowUp[] {
    return (input ?? []).map((f) => ({ id: f.id ?? randomUUID(), title: f.title, description: f.description }));
  }

  // ---- ToolHost ----

  async recordDecision(input: RecordDecisionInput, call: CallContext): Promise<ToolOutcome> {
    const lane = await this.attribute(call, input, "record_decision");
    this.emitter.emit({
      type: "node.decision",
      title: input.title,
      rationale: input.rationale,
      affects: input.affects ?? [],
      reversible: input.reversible ?? true,
      ...(lane.branchId ? { branch_id: lane.branchId } : {}),
    });
    return { ok: true, text: "Decision recorded." };
  }

  async askUser(input: AskUserInput, call: CallContext): Promise<ToolOutcome> {
    if ((await this.attribute(call, input, "ask_user")).inSubagent) return { ok: false, error: SUBAGENT_TOOL_ERROR };
    const rejected = this.claimTerminal("ask_user");
    if (rejected) return { ok: false, error: rejected };
    const questionId = randomUUID();
    const answer = new Promise<string>((resolve, reject) => this.questions.set(questionId, { resolve, reject }));
    this.emitter.emit({
      type: "node.question",
      question_id: questionId,
      question: input.question,
      options: input.options ?? [],
      allow_free_text: input.allow_free_text ?? true,
    });
    try {
      const text = await answer;
      return { ok: true, text: `The user answered: ${text}` };
    } catch (e) {
      return { ok: false, error: (e as Error).message };
    } finally {
      // The answer resumes the same round; it still needs its own terminal call.
      if (this.round?.terminal === "ask_user") this.round.terminal = null;
    }
  }

  async writeSummary(input: WriteSummaryInput, call: CallContext): Promise<ToolOutcome> {
    if ((await this.attribute(call, input, "write_summary")).inSubagent) {
      return { ok: false, error: SUBAGENT_TOOL_ERROR };
    }
    const rejected = this.claimTerminal("write_summary");
    if (rejected) return { ok: false, error: rejected };
    this.emitter.emit({
      type: "node.summary",
      done: input.done,
      remaining: input.remaining,
      follow_ups: this.followUps(input.follow_ups),
      ...(input.notes ? { notes: input.notes } : {}),
    });
    return { ok: true, text: "Summary recorded; the section is closed. End your turn now." };
  }

  async recordOutcome(input: RecordOutcomeInput, call: CallContext): Promise<ToolOutcome> {
    if ((await this.attribute(call, input, "record_outcome")).inSubagent) {
      return { ok: false, error: SUBAGENT_TOOL_ERROR };
    }
    const rejected = this.claimTerminal("record_outcome");
    if (rejected) return { ok: false, error: rejected };
    this.emitter.emit({
      type: "node.outcome",
      kind: input.kind,
      ref: input.ref,
      summary: input.summary,
      follow_ups: this.followUps(input.follow_ups),
    });
    return { ok: true, text: "Outcome recorded; the workflow is complete. End your turn now." };
  }

  async presentArtifact(input: PresentArtifactInput, call: CallContext): Promise<ToolOutcome> {
    if ((await this.attribute(call, input, "present_artifact")).inSubagent) {
      return { ok: false, error: SUBAGENT_TOOL_ERROR };
    }
    const path = isAbsolute(input.path) ? input.path : resolve(this.cwd, input.path);
    let size: number;
    try {
      const st = statSync(path);
      if (!st.isFile()) return { ok: false, error: `Not a file: ${path}` };
      size = st.size;
    } catch {
      return { ok: false, error: `No file at ${path}. Save it first, then call present_artifact.` };
    }
    this.emitter.emit({
      type: "node.artifact",
      artifact_id: randomUUID(),
      path,
      title: input.title,
      ...(input.description ? { description: input.description } : {}),
      size_bytes: size,
    });
    return { ok: true, text: `Shown to the user as "${input.title}".` };
  }

  async requestSlot(input: RequestSlotInput, call: CallContext): Promise<ToolOutcome> {
    if ((await this.attribute(call, input, "request_slot")).inSubagent) {
      return { ok: false, error: SUBAGENT_TOOL_ERROR };
    }
    const requestId = randomUUID();
    const resolved = new Promise<RequestSlotResult>((resolve, reject) =>
      this.slotRequests.set(requestId, { resolve, reject }),
    );
    this.emitter.emit({
      type: "slot.request",
      request_id: requestId,
      name: input.name,
      purpose: input.purpose,
      ...(input.from_follow_up ? { from_follow_up: input.from_follow_up } : {}),
    });
    const r = await resolved;
    return { ok: true, text: JSON.stringify(r) };
  }
}
