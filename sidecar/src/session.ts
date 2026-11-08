// Agent SDK session lifecycle, round tracking, and nudges (AGENTS.md §5.3, §10).
//
// The sidecar is stateless across restarts: everything it learns is emitted to
// the app, which persists it. It owns only what must happen in-process: blocking
// tool calls, branch attribution from the stream, and the nudge loop.

import { readFileSync, statSync } from "node:fs";

import {
  query as sdkQuery,
  type AccountInfo,
  type HookInput,
  type HookJSONOutput,
  type ModelInfo,
  type Options,
  type SDKMessage,
  type SlashCommand,
  type SDKUserMessage,
} from "@anthropic-ai/claude-agent-sdk";

import { AGENT_TOOLS, BranchTracker, agentResult, taskStatus, type Branch } from "./branches.js";
import type { Logger } from "./log.js";
import type { Emitter } from "./ndjson.js";
import type { Effort, JsonValue, SessionResume, SessionStart, TurnAttachment } from "./protocol.js";
import { AsyncQueue } from "./queue.js";
import { TaskTracker } from "./tasks.js";
import {
  type ContextReport,
  type UsageReport,
  contextCategories,
  contextWindow,
  quotaFromRateLimit,
  quotaFromUsage,
} from "./usage.js";
import { BerthHost, SLASH_COMMAND, artifactLink, type Lane } from "./host.js";
import { berthSystemPrompt, skillsPrompt, subagentLimitText, type AgentLimits } from "./prompt.js";
import { berthToolName, createBerthServer, type CallContext } from "./tools.js";
import { VERSION } from "./version.js";

/** Claude Code's tool that publishes pages to claude.ai. */
const ARTIFACT_TOOL = "Artifact";

export { MAX_NUDGES, NUDGE_TEXT, SUBAGENT_TOOL_ERROR, artifactLink } from "./host.js";
export { BERTH_SYSTEM_PROMPT, berthSystemPrompt, subagentLimitText, type AgentLimits } from "./prompt.js";
/** Largest attached image sent to Claude as an image block (larger ones go by path only). */
const MAX_IMAGE_BYTES = 5 * 1024 * 1024;
const IMAGE_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp"] as const;
type ImageType = (typeof IMAGE_TYPES)[number];
/** How long a tool handler waits for its tool_use to appear in the stream. */
const ATTRIBUTION_WAIT_MS = 3000;
/** How often the plan's usage is asked for, at most (it is account-wide). */
const QUOTA_EVERY_MS = 60_000;

/** The subset of the SDK's `Query` the session uses (lets tests supply a fake). */
export interface AgentQuery extends AsyncIterable<SDKMessage> {
  interrupt(): Promise<unknown>;
  close(): void;
  setModel(model?: string): Promise<void>;
  applyFlagSettings(settings: { effortLevel?: Effort | null }): Promise<void>;
  supportedModels(): Promise<ModelInfo[]>;
  /** Optional so test fakes may leave them out; the SDK's `Query` has them. */
  supportedCommands?(): Promise<SlashCommand[]>;
  accountInfo?(): Promise<AccountInfo>;
  backgroundTasks?(toolUseId?: string): Promise<boolean>;
  stopTask?(taskId: string): Promise<void>;
  /** At runtime on the SDK's `Query` (the `get_task_output` control), not
   * in its published types. */
  getTaskOutput?(taskId: string): Promise<{ output: string; total_bytes: number; truncated: boolean }>;
  getContextUsage?(opts?: { detail?: "summary" | "full" }): Promise<ContextReport>;
  usage_EXPERIMENTAL_MAY_CHANGE_DO_NOT_RELY_ON_THIS_API_YET?(opts?: { skipBehaviors?: boolean }): Promise<UsageReport>;
}

const EFFORTS: readonly Effort[] = ["low", "medium", "high", "xhigh", "max"];

export type QueryFn = (params: { prompt: AsyncIterable<SDKUserMessage>; options: Options }) => AgentQuery;

export const defaultQuery: QueryFn = (params) => sdkQuery(params);

/** `BERTH_CLAUDE_PATH` (the app sets it to the Claude Code it installed), else the SDK's own binary. */
export function claudePath(): string | undefined {
  return process.env.BERTH_CLAUDE_PATH || undefined;
}

/** How Claude Code's refusal of a guarded resume starts (`resumeDropsTurn`). */
export const RESUME_REJECTED = "Resume rejected by --resume-drops-turn:";

interface ObservedToolUse {
  id: string;
  name: string;
  input: unknown;
  parentToolUseId: string | null;
  claimed: boolean;
}

export interface SessionDeps {
  emitter: Emitter;
  log: Logger;
  query?: QueryFn;
  now?: () => number;
}

export class Session extends BerthHost {
  private readonly queryFn: QueryFn;
  private input = new AsyncQueue<SDKUserMessage>();
  /** How the session started, for a plain resume when Claude Code refuses
   * a rewind's guarded one (ADR 0016). */
  private launch?: { msg: SessionStart | SessionResume; resume?: string };
  /** Turns sent since a guarded resume, until it is known to have gone
   * through: a refused one sends them again. */
  private replay?: SDKUserMessage[];
  private readonly branches: BranchTracker;
  private readonly tasks: TaskTracker;
  private readonly toolUses = new Map<string, ObservedToolUse>();
  private readonly toolUseWaiters = new Map<string, Array<(t: ObservedToolUse) => void>>();
  /** The most subagents at once, as the app said at the start (ADR 0032). */
  private maxSubagents?: number;
  /** Agent calls the limit let through, until their branch collects. */
  private readonly admitted = new Set<string>();
  /** Agent calls the limit held back, with what the agent read. */
  private readonly heldBack = new Map<string, string>();

  private q?: AgentQuery;
  private consuming?: Promise<void>;
  private startReplyTo?: string;
  private claudeSessionId?: string;
  private seq = 0;
  /** A context report is being made; `contextAgain` asks for one more after it. */
  private contextBusy = false;
  private contextAgain = false;
  private quotaAt = -Infinity;
  /** Commands bound to Claude Code's terminal (from `init`), sent with the list. */
  private terminalCommands: string[] = [];

  constructor(deps: SessionDeps) {
    super(deps.emitter, deps.log, deps.now ?? Date.now);
    this.queryFn = deps.query ?? defaultQuery;
    this.branches = new BranchTracker(this.now);
    this.tasks = new TaskTracker(this.now);
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
    const resumeAt = "resume_at" in msg ? msg.resume_at : undefined;
    const dropsTurn = "drops_turn" in msg && resumeAt ? msg.drops_turn : undefined;
    if (resume) this.claudeSessionId = resume;
    this.launch = { msg, resume };
    if (dropsTurn) this.replay = [];
    this.q = this.queryFn({ prompt: this.input, options: this.options(msg, resume, resumeAt, dropsTurn) });
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

  /** Tell the app which models (and efforts) it can offer: each one by the
   * exact model it is, not an alias ("opus" moves with every release). */
  private async reportModels(q: AgentQuery): Promise<void> {
    try {
      const seen = new Set<string>();
      const models = [];
      for (const m of await q.supportedModels()) {
        const value = m.resolvedModel ?? m.value;
        // The app has its own Default; aliases of one model collapse into it.
        if (m.value === "default" || seen.has(value)) continue;
        seen.add(value);
        models.push({
          value,
          display_name: m.displayName,
          description: m.description,
          efforts: (m.supportedEffortLevels ?? []).filter((e): e is Effort => EFFORTS.includes(e as Effort)),
        });
      }
      if (models.length > 0) this.emitter.emit({ type: "session.models", models });
    } catch (e) {
      this.log.warn({ err: e }, "supportedModels failed");
    }
  }

  turn(text: string, attachments: TurnAttachment[] = [], uuid?: string): void {
    if (!this.q) {
      this.error("session.turn before session.start", false);
      return;
    }
    // A turn sent while a round runs joins it (ADR 0018): Claude Code folds
    // it into the running turn between tool rounds, or runs it right after.
    if (!this.round) {
      this.round = this.newRound();
      this.round.command = SLASH_COMMAND.test(text.trimStart());
    }
    this.push(text, attachments, uuid);
  }

  /** Move running foreground tool calls to the background (all of them, or
   * the one `toolUseId` names). */
  async background(toolUseId?: string): Promise<void> {
    if (!this.q?.backgroundTasks) {
      this.error("Nothing to move to the background: no session.", false);
      return;
    }
    try {
      const moved = await this.q.backgroundTasks(toolUseId);
      if (!moved) this.error("Nothing was running in the foreground to move.", false);
    } catch (e) {
      this.error(`Could not move it to the background: ${e instanceof Error ? e.message : String(e)}`, false);
    }
  }

  async interrupt(): Promise<void> {
    if (!this.q || !this.round) return;
    this.round.interrupted = true;
    this.abandonPending("Interrupted by the user.", "Interrupted by the user before the app confirmed delivery.");
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






  // ---- SDK stream ----

  private options(
    msg: SessionStart,
    resume: string | undefined,
    resumeAt: string | undefined,
    dropsTurn: string | undefined,
  ): Options {
    const skills = skillsPrompt(msg.connector_skills);
    const limits: AgentLimits = {
      slotMessages: msg.slot_messages !== false,
      ...(msg.max_subagents === undefined ? {} : { maxSubagents: msg.max_subagents }),
    };
    this.maxSubagents = limits.maxSubagents;
    const append = [berthSystemPrompt(limits) + skills, msg.system_prompt]
      .filter((s) => s.trim().length > 0)
      .join("\n\n");
    return {
      cwd: msg.cwd,
      systemPrompt: { type: "preset", preset: "claude_code", append, snapshot: true },
      // Always auto-approve (§1.6): isolation is the slot's job.
      permissionMode: "bypassPermissions",
      allowDangerouslySkipPermissions: true,
      mcpServers: { berth: createBerthServer(this, { slotMessages: limits.slotMessages }) },
      allowedTools: ["mcp__berth__*", ...msg.allowed_tools],
      // Without subagents, Claude Code's Agent tool isn't on offer (ADR 0032).
      disallowedTools: ["AskUserQuestion", ...(limits.maxSubagents === 0 ? AGENT_TOOLS : [])],
      // Up to a number, Berth counts them as they start (ADR 0032).
      ...(limits.maxSubagents
        ? {
            hooks: {
              PreToolUse: [{ matcher: [...AGENT_TOOLS].join("|"), hooks: [async (input) => this.gateSubagent(input)] }],
            },
          }
        : {}),
      skills: "all",
      env: { ...process.env, ...msg.env, CLAUDE_AGENT_SDK_CLIENT_APP: `berth/${VERSION}` },
      ...(resume ? { resume } : {}),
      // A rewound conversation goes on from this entry; later ones are dropped
      // (ADR 0009). When it drops exactly one turn, Claude Code checks that
      // nothing else goes with it (ADR 0016).
      ...(resume && resumeAt ? { resumeSessionAt: resumeAt } : {}),
      ...(resume && resumeAt && dropsTurn ? { resumeDropsTurn: dropsTurn } : {}),
      // A new session's title in Claude Code's history; a resumed one keeps its own.
      ...(msg.title && !resume ? { title: msg.title } : {}),
      // What each subagent is doing, every ~30s; its own words in "Details";
      // a suggested next prompt after each round (ADR 0016).
      agentProgressSummaries: true,
      forwardSubagentText: true,
      promptSuggestions: true,
      // Berth stops tasks one at a time (ADR 0022): an interrupt ends the
      // turn and spares what runs in the background.
      perTaskStopAffordance: true,
      ...(msg.model ? { model: msg.model } : {}),
      ...(msg.effort ? { effort: msg.effort } : {}),
      ...(claudePath() ? { pathToClaudeCodeExecutable: claudePath() } : {}),
      stderr: (data: string) => this.log.debug({ claude: data.trimEnd() }, "claude stderr"),
    };
  }

  private async consume(q: AgentQuery): Promise<void> {
    try {
      for await (const msg of q) {
        // A query replaced by a plain resume: what it still says is moot.
        if (q !== this.q) return;
        this.handle(msg);
      }
    } catch (e) {
      if (q !== this.q) return;
      this.error(`agent stream failed: ${(e as Error).message}`, true);
    } finally {
      this.abandonPending("Session ended.", "The session ended before the app confirmed delivery.");
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
        if (msg.subtype === "init") this.onInit(msg.session_id, msg.terminal_slash_commands ?? []);
        else if (msg.subtype === "task_notification") {
          this.onTaskNotification(msg);
          this.emitTasks(this.tasks.ended(msg.task_id));
        } else if (msg.subtype === "task_started") this.emitTasks(this.tasks.started(msg));
        else if (msg.subtype === "task_updated") this.emitTasks(this.tasks.updated(msg));
        else if (msg.subtype === "compact_boundary") this.onCompacted(msg.compact_metadata);
        else if (msg.subtype === "commands_changed") this.emitCommands(msg.commands);
        else if (msg.subtype === "local_command_output") this.onCommandOutput(msg.content);
        else if (msg.subtype === "task_progress") {
          this.onTaskProgress(msg.tool_use_id, msg.summary);
          this.emitTasks(this.tasks.progress(msg));
        } else if (msg.subtype === "background_tasks_changed") this.emitTasks(this.tasks.background(msg.tasks));
        else if (msg.subtype === "model_refusal_fallback" || msg.subtype === "model_refusal_no_fallback")
          this.onRefusal(msg);
        break;
      case "prompt_suggestion":
        if (msg.suggestion.trim()) this.emitter.emit({ type: "session.suggestion", text: msg.suggestion.trim() });
        break;
      case "rate_limit_event": {
        const quota = quotaFromRateLimit(msg.rate_limit_info);
        if (quota) this.emitter.emit({ type: "session.quota", ...quota });
        void this.reportQuota();
        break;
      }
      case "assistant":
        // The guarded resume went through.
        if (!parent) this.replay = undefined;
        this.onAssistant(msg.message.id, msg.message.content, parent, branch);
        break;
      case "user":
        if ("isReplay" in msg && msg.isReplay) this.onReplay(msg.message.content);
        else this.onUser(msg.message.content, msg.tool_use_result);
        break;
      case "result":
        if (msg.subtype === "error_during_execution" && msg.errors.some((e) => e.startsWith(RESUME_REJECTED))) {
          this.resumePlainly();
          break;
        }
        this.replay = undefined;
        this.onResult(
          msg.total_cost_usd,
          msg.subtype !== "success" || msg.is_error,
          msg.num_turns,
          msg.queued_turn_count ?? 0,
        );
        break;
      default:
        break;
    }
  }

  private onInit(sessionId: string, terminalCommands: string[]): void {
    // Reply once to session.start/resume, and again only if the id changes.
    if (this.startReplyTo === undefined && sessionId === this.claudeSessionId) return;
    const first = this.startReplyTo !== undefined;
    this.claudeSessionId = sessionId;
    this.emitter.emit({ type: "session.started", claude_session_id: sessionId }, this.startReplyTo);
    this.startReplyTo = undefined;
    // A resumed conversation already fills part of the window.
    void this.reportContext();
    void this.reportQuota();
    if (first) {
      this.terminalCommands = terminalCommands;
      void this.reportCommands();
      void this.reportAccount();
    }
  }

  /** Who Claude Code works as, for the rail's footer (ADR 0016). */
  private async reportAccount(): Promise<void> {
    const q = this.q;
    if (!q?.accountInfo) return;
    try {
      const a = await q.accountInfo();
      this.emitter.emit({
        type: "session.account",
        ...(a.email ? { email: a.email } : {}),
        ...(a.organization ? { organization: a.organization } : {}),
        ...(a.subscriptionType ? { plan: a.subscriptionType } : {}),
        ...(a.apiProvider ? { provider: a.apiProvider } : {}),
      });
    } catch (e) {
      this.log.warn({ err: e }, "accountInfo failed");
    }
  }

  /** A subagent's progress summary, on its branch. */
  private onTaskProgress(toolUseId: string | undefined, summary: string | undefined): void {
    const branch = toolUseId ? this.branches.get(toolUseId) : undefined;
    const text = summary?.trim();
    if (branch && text) this.emitter.emit({ type: "branch.progress", branch_id: branch.branchId, summary: text });
  }

  /** The session's live tasks, when they changed (ADR 0022). */
  private emitTasks(changed: boolean): void {
    if (changed) this.emitter.emit({ type: "session.tasks", tasks: this.tasks.list() });
  }

  /** Stop one task; it leaves the list once Claude Code reports it stopped. */
  async stopTask(taskId: string): Promise<void> {
    if (!this.q?.stopTask) {
      this.error("Nothing to stop: no session.", false);
      return;
    }
    try {
      await this.q.stopTask(taskId);
    } catch (e) {
      this.error(`Could not stop it: ${e instanceof Error ? e.message : String(e)}`, false);
    }
  }

  /** The end of a shell or monitor task's output, for the app. */
  async readTask(taskId: string): Promise<void> {
    const reply = (r: { output: string; total_bytes: number; truncated: boolean }, error?: string) =>
      this.emitter.emit({ type: "task.output", task_id: taskId, ...r, ...(error ? { error } : {}) });
    const none = { output: "", total_bytes: 0, truncated: false };
    if (!this.q?.getTaskOutput) {
      reply(none, "This session can't read a task's output.");
      return;
    }
    try {
      reply(await this.q.getTaskOutput(taskId));
    } catch (e) {
      reply(none, e instanceof Error ? e.message : String(e));
    }
  }

  private onRefusal(msg: {
    subtype: string;
    original_model: string;
    fallback_model?: string;
    scope?: "session" | "local";
    api_refusal_category?: string | null;
    api_refusal_explanation?: string | null;
  }): void {
    this.emitter.emit({
      type: "node.refusal",
      model: msg.original_model,
      ...(msg.subtype === "model_refusal_fallback" && msg.fallback_model ? { fallback_model: msg.fallback_model } : {}),
      ...(msg.api_refusal_category ? { category: msg.api_refusal_category } : {}),
      ...(msg.api_refusal_explanation ? { explanation: msg.api_refusal_explanation } : {}),
      subagent: msg.scope === "local",
    });
  }

  /** Claude Code refused the rewind's guarded resume: something besides the
   * dropped turn would have gone (a queued notification). As the SDK asks,
   * resume the whole conversation instead and send the turns again; the
   * prompt's rewind note tells the agent what was dropped (ADR 0016). */
  private resumePlainly(): void {
    const launch = this.launch;
    const old = this.q;
    if (!launch || !old) return;
    const replay = this.replay ?? [];
    this.replay = undefined;
    this.log.warn("guarded resume refused; resuming the whole conversation");
    this.input = new AsyncQueue<SDKUserMessage>();
    this.q = this.queryFn({ prompt: this.input, options: this.options(launch.msg, launch.resume, undefined, undefined) });
    old.close();
    this.consuming = this.consume(this.q);
    for (const m of replay) this.input.push(m);
    this.error(
      "Claude Code could not rewind exactly (something else happened in the dropped turn), so the agent resumed the whole conversation and was told what was dropped.",
      false,
    );
  }

  /** Tell the app which slash commands and skills the session offers (ADR 0015). */
  private async reportCommands(): Promise<void> {
    const q = this.q;
    if (!q?.supportedCommands) return;
    try {
      this.emitCommands(await q.supportedCommands());
    } catch (e) {
      this.log.warn({ err: e }, "supportedCommands failed");
    }
  }

  private emitCommands(commands: SlashCommand[]): void {
    this.emitter.emit({
      type: "session.commands",
      commands: commands.map((c) => ({
        name: c.name,
        description: c.description,
        argument_hint: c.argumentHint,
        aliases: c.aliases ?? [],
        builtin: c.builtin ?? false,
      })),
      terminal: this.terminalCommands,
    });
  }

  private onCompacted(meta?: { trigger: "manual" | "auto"; pre_tokens: number; post_tokens?: number }): void {
    void this.reportContext();
    if (!meta) return;
    this.emitter.emit({
      type: "node.compacted",
      trigger: meta.trigger,
      pre_tokens: meta.pre_tokens,
      ...(meta.post_tokens !== undefined ? { post_tokens: meta.post_tokens } : {}),
    });
  }

  /** What a slash command printed: kept as the round's last text, which the
   * app shows when the round turns out to be the command alone. */
  private onCommandOutput(text: string): void {
    const out = text.trim();
    if (out && this.round) this.round.lastText = out;
  }

  /** Claude Code replays a local command's output as a user message:
   * `<local-command-stdout>…</local-command-stdout>` (or stderr). */
  private onReplay(content: unknown): void {
    if (typeof content !== "string") return;
    const m = /<local-command-(?:stdout|stderr)>([\s\S]*?)<\/local-command-(?:stdout|stderr)>/.exec(content);
    if (m?.[1] !== undefined) this.onCommandOutput(m[1]);
  }

  /** Tell the app how full the context window is (ADR 0014). One report at a
   * time; one asked for meanwhile follows it. */
  private async reportContext(): Promise<void> {
    const q = this.q;
    if (!q?.getContextUsage) return;
    if (this.contextBusy) {
      this.contextAgain = true;
      return;
    }
    this.contextBusy = true;
    try {
      do {
        this.contextAgain = false;
        // `summary` answers from the last response's usage: no token-count calls.
        const r = await q.getContextUsage({ detail: "summary" });
        this.emitter.emit({ type: "session.context", categories: contextCategories(r), ...contextWindow(r) });
      } while (this.contextAgain);
    } catch (e) {
      this.log.warn({ err: e }, "getContextUsage failed");
    } finally {
      this.contextBusy = false;
    }
  }

  /** Tell the app where the plan's limits stand, at most once a minute. The
   * SDK marks the call experimental; it is pinned, and without it the app
   * still has the rate-limit events. */
  private async reportQuota(): Promise<void> {
    const q = this.q;
    const usage = q?.usage_EXPERIMENTAL_MAY_CHANGE_DO_NOT_RELY_ON_THIS_API_YET;
    if (!q || !usage) return;
    const now = this.now();
    if (now - this.quotaAt < QUOTA_EVERY_MS) return;
    this.quotaAt = now;
    try {
      const quota = quotaFromUsage(await usage.call(q, { skipBehaviors: true }));
      if (quota) this.emitter.emit({ type: "session.quota", ...quota });
    } catch (e) {
      this.log.warn({ err: e }, "usage report failed");
    }
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
      if (!block.is_error && this.toolUses.get(block.tool_use_id)?.name === ARTIFACT_TOOL) {
        this.notePublish(toolUseResult);
      }
      const branch = this.branches.get(block.tool_use_id);
      if (!branch || branch.collected) continue;
      // Never started: the subagent limit held it back (ADR 0032).
      const held = this.heldBack.get(block.tool_use_id);
      if (held !== undefined) {
        this.heldBack.delete(block.tool_use_id);
        this.emitter.emit({
          type: "branch.collected",
          ...this.branches.collect(branch, { resultText: held, status: "cancelled", durationMs: 0, toolCalls: 0 }),
        });
        continue;
      }
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

  /** A publish result carries the page's link and title; other actions don't. */
  private notePublish(result: unknown): void {
    if (typeof result !== "object" || result === null) return;
    const { url, path, title } = result as { url?: unknown; path?: unknown; title?: unknown };
    if (typeof url !== "string" || typeof path !== "string") return;
    this.published.set(artifactLink(url) ?? url, typeof title === "string" && title.trim() ? title : undefined);
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

  private onResult(totalUsd: number, isError: boolean, numTurns: number, queued: number): void {
    if (Number.isFinite(totalUsd)) this.totalUsd = totalUsd;
    this.emitter.emit({ type: "session.cost", total_usd: this.totalUsd });
    void this.reportContext();
    void this.reportQuota();
    this.settleRound(isError, numTurns, queued);
  }

  protected nudge(text: string): void {
    this.push(text);
  }

  protected override onRoundEnd(): void {
    // A call let through that never showed up in the stream isn't running.
    for (const id of this.admitted) if (!this.branches.get(id)) this.admitted.delete(id);
    this.heldBack.clear();
  }

  /** Which lane a tool call belongs to, from the stream (§5.4). */
  protected lane(call: CallContext, input: unknown, tool: string): Promise<Lane> {
    return this.attribute(call, input, tool);
  }

  /** A PreToolUse hook on Claude Code's Agent tool: let it start while
   * fewer than the limit run, else hold it back with why (ADR 0032). A
   * subagent runs from its call until its branch collects, in the background
   * too. */
  gateSubagent(input: HookInput): HookJSONOutput {
    const max = this.maxSubagents;
    if (max === undefined || input.hook_event_name !== "PreToolUse" || !AGENT_TOOLS.has(input.tool_name)) return {};
    for (const id of this.admitted) if (this.branches.get(id)?.collected) this.admitted.delete(id);
    if (this.admitted.size < max) {
      this.admitted.add(input.tool_use_id);
      return {};
    }
    const why = subagentLimitText(max);
    this.heldBack.set(input.tool_use_id, why);
    return {
      hookSpecificOutput: { hookEventName: "PreToolUse", permissionDecision: "deny", permissionDecisionReason: why },
    };
  }



  private push(text: string, attachments: TurnAttachment[] = [], uuid?: string): void {
    const images = attachments.flatMap((a) => this.imageBlock(a));
    const message: SDKUserMessage = {
      type: "user",
      message: {
        role: "user",
        content: images.length > 0 ? [{ type: "text", text }, ...images] : text,
      },
      parent_tool_use_id: null,
      // The transcript keeps it, so a rewind can name this turn (ADR 0016).
      ...(uuid ? { uuid: uuid as `${string}-${string}-${string}-${string}-${string}` } : {}),
      ...(this.claudeSessionId ? { session_id: this.claudeSessionId } : {}),
    };
    this.replay?.push(message);
    this.input.push(message);
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

  private async attribute(call: CallContext, input: unknown, tool: string): Promise<Lane> {
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



  // ---- ToolHost ----









}
