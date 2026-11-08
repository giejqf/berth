// Agent SDK session lifecycle, round tracking, and nudges (AGENTS.md §5.3, §10).
//
// The sidecar is stateless across restarts: everything it learns is emitted to
// the app, which persists it. It owns only what must happen in-process: blocking
// tool calls, branch attribution from the stream, and the nudge loop.

import { randomUUID } from "node:crypto";
import { readFileSync, statSync } from "node:fs";
import { isAbsolute, resolve } from "node:path";

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
import type {
  AskUserInput,
  Effort,
  FollowUp,
  FollowUpInput,
  JsonValue,
  MessageSlotInput,
  PresentArtifactInput,
  PresentReviewInput,
  RecordDecisionInput,
  RecordOutcomeInput,
  ReplyInput,
  RequestSlotInput,
  RequestSlotResult,
  SessionResume,
  SessionStart,
  TerminalKind,
  TurnAttachment,
  WriteSummaryInput,
} from "./protocol.js";
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
import { berthToolName, createBerthServer, type CallContext, type ToolHost, type ToolOutcome } from "./tools.js";
import { VERSION } from "./version.js";

/** What Settings lets the agent do beyond its slot (ADR 0032). */
export interface AgentLimits {
  /** `message_slot` is on offer. */
  slotMessages: boolean;
  /** The most subagents at once; 0, none; undefined, no limit of Berth's. */
  maxSubagents?: number;
}

/** Berth's part of the system prompt, for what the agent may do. */
export function berthSystemPrompt(limits: AgentLimits): string {
  const max = limits.maxSubagents;
  return [
    BERTH_PROMPT_HEAD,
    ...(limits.slotMessages ? [MESSAGE_SLOT_PROMPT] : []),
    BERTH_PROMPT_TAIL,
    ...(max === undefined || max === 0
      ? []
      : [
          `- Run at most ${max} ${max === 1 ? "subagent" : "subagents"} at a time (the user's setting in Berth); start another once one has finished.`,
        ]),
    // Without subagents, Claude Code's Agent tool isn't on offer either.
    ...(max === 0 ? [] : [SUBAGENT_PROMPT]),
  ].join("\n");
}

const BERTH_PROMPT_HEAD = `You are working inside Berth. The user never reads or edits code: they follow your decisions, answer your questions, review diffs, and ship pull requests.

How to work:
- Announce each meaningful decision with the berth \`record_decision\` tool the moment you make it (an approach chosen, a trade-off accepted, a convention adopted). Short title; the why goes in the rationale.
- Never narrate file edits or tool use in prose. The user reviews the diff separately.
- End every round with exactly one of these berth tools: \`ask_user\` when you need the user's input, \`write_summary\` when you finished a chunk of work for the user to review, \`reply\` when you answered a question or discussed something without changing files for them to review, or \`record_outcome\` when the workflow is done (usually right after opening a PR). Never end a turn without one of them, and never call two.
- The user reads only the berth tools' content, not your other text. When they ask you something, the answer goes in \`reply\` (or in \`write_summary\`'s notes when you also changed files), complete and in Markdown.
- Ask questions only through \`ask_user\`, never through AskUserQuestion or plain text.
- Use \`gh\` for GitHub. Open PRs with \`gh pr create\`, then call \`record_outcome\` with kind "pr" and the PR URL as ref.
- When the workflow's result is a page you published with Claude Code's \`Artifact\` tool rather than code to merge, end it with \`record_outcome\` kind "claude_artifact" and the claude.ai link the publish returned as ref. (\`present_artifact\` only shows the user a file; it publishes nothing.)
- Use the connector CLIs described by their skills for external systems (Linear, PostHog, ...).
- Run the repository's gates (tests, lint, typecheck) before \`write_summary\` when you changed code.
- When you review code (because the user asked, or a reviewer subagent reported back), present the findings with \`present_review\` rather than listing them in prose or a summary, then act on the decisions it returns: fix what the user marked fix, leave the dismissed ones, follow their words on the rest.`;

const MESSAGE_SLOT_PROMPT = `- Other slots in this repository may be building other parts of the same thing (the backend to your frontend). When their work needs to know something (a bug you found in their part, an interface you need from them or changed for them), tell them with \`message_slot\` rather than changing their code, then carry on with your own work. A turn that starts with "Message from slot" is another slot's agent writing to you: act on it as you would on the user's request, answer with \`message_slot\` when it needs an answer, and end the round as usual.`;

const BERTH_PROMPT_TAIL = `- When you make something the user should see (a screenshot, a screen recording, a rendered chart or page, a report), save it to a file and call \`present_artifact\` with its path and a short title. Never paste images or long output into text.
- Files the user attached are listed at the end of their message, with paths you can read; attached images are also shown to you directly.`;

const SUBAGENT_PROMPT = "- Subagents may only call `record_decision`; they report everything else back to you.";

/** The whole of it, with every tool on offer and no subagent limit. */
export const BERTH_SYSTEM_PROMPT = berthSystemPrompt({ slotMessages: true });

/** What an agent reads when the subagent limit holds a spawn back. */
export function subagentLimitText(max: number): string {
  const what = max === 1 ? "one subagent at a time" : `${max} subagents at a time`;
  return `Not started: the user's setting in Berth allows ${what}, and that many are running. Start it once one has finished, or do this part yourself.`;
}

/** Claude Code's tool that publishes pages to claude.ai. */
const ARTIFACT_TOOL = "Artifact";
const ARTIFACT_REF_ERROR =
  "For kind claude_artifact, ref must be the artifact's claude.ai link (https://claude.ai/artifact/… or https://claude.ai/code/artifact/…), as the Artifact tool's publish returned it.";

/** A claude.ai artifact link in one form (https, no query, no trailing
 * slash), or undefined when `ref` is not one. */
export function artifactLink(ref: string): string | undefined {
  const raw = ref.trim();
  let url: URL;
  try {
    url = new URL(/^claude\.ai\//.test(raw) ? `https://${raw}` : raw);
  } catch {
    return undefined;
  }
  const path = url.pathname.replace(/\/+$/, "");
  if (url.protocol !== "https:" || url.hostname !== "claude.ai") return undefined;
  if (!/^\/(code\/)?artifact\/[^/]+$/.test(path)) return undefined;
  return `https://claude.ai${path}`;
}

export const NUDGE_TEXT = "End this round with ask_user, write_summary, reply, or record_outcome.";
export const MAX_NUDGES = 2;
export const SUBAGENT_TOOL_ERROR = "Not available to subagents; report this back to the main agent.";
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

type TerminalTool = "ask_user" | "present_review" | "write_summary" | "record_outcome" | "reply";

interface Round {
  startedAt: number;
  startTotalUsd: number;
  nudges: number;
  /** Accepted terminal call; `ask_user` and `present_review` clear again once answered (§5.3). */
  terminal: TerminalTool | null;
  interrupted: boolean;
  lastText?: string;
  /** The turn that opened it was a slash command (`/name`). */
  command?: boolean;
}

/** How Claude Code's refusal of a guarded resume starts (`resumeDropsTurn`). */
export const RESUME_REJECTED = "Resume rejected by --resume-drops-turn:";

/** `/name` at the start of a turn: what Claude Code would run as a command. */
const SLASH_COMMAND = /^\/[A-Za-z0-9][A-Za-z0-9:._-]*(?:\s|$)/;

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
  private readonly questions = new Map<string, Pending<string>>();
  private readonly reviews = new Map<string, Pending<string>>();
  private readonly slotRequests = new Map<string, Pending<RequestSlotResult>>();
  /** `message_slot` calls waiting for the app to say where the message went. */
  private readonly slotMessages = new Map<string, Pending<ToolOutcome>>();
  /** claude.ai artifacts this session published with the Artifact tool:
   * link → title, for an Outcome that names one (ADR 0020). */
  private readonly published = new Map<string, string | undefined>();
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
  private round?: Round;
  private cwd = "";
  private seq = 0;
  private totalUsd = 0;
  /** A context report is being made; `contextAgain` asks for one more after it. */
  private contextBusy = false;
  private contextAgain = false;
  private quotaAt = -Infinity;
  /** Commands bound to Claude Code's terminal (from `init`), sent with the list. */
  private terminalCommands: string[] = [];

  constructor(deps: SessionDeps) {
    this.emitter = deps.emitter;
    this.log = deps.log;
    this.queryFn = deps.query ?? defaultQuery;
    this.now = deps.now ?? Date.now;
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
    for (const pending of [this.questions, this.reviews]) {
      for (const [id, p] of pending) {
        p.reject(new Error("Interrupted by the user."));
        pending.delete(id);
      }
    }
    for (const [id, p] of this.slotRequests) {
      p.resolve({ approved: false });
      this.slotRequests.delete(id);
    }
    this.settleSlotMessages("Interrupted by the user before the app confirmed delivery.");
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

  answerReview(reviewId: string, text: string): void {
    const pending = this.reviews.get(reviewId);
    if (!pending) {
      this.error(`no pending review ${reviewId}`, false);
      return;
    }
    this.reviews.delete(reviewId);
    pending.resolve(text);
  }

  /** The app routed a `message_slot` call: its result is what the agent reads. */
  resolveSlotMessage(requestId: string, ok: boolean, text: string): void {
    const pending = this.slotMessages.get(requestId);
    if (!pending) {
      this.error(`no pending slot message ${requestId}`, false);
      return;
    }
    this.slotMessages.delete(requestId);
    pending.resolve(ok ? { ok: true, text } : { ok: false, error: text });
  }

  private settleSlotMessages(why: string): void {
    for (const p of this.slotMessages.values()) p.resolve({ ok: false, error: why });
    this.slotMessages.clear();
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

  private options(
    msg: SessionStart,
    resume: string | undefined,
    resumeAt: string | undefined,
    dropsTurn: string | undefined,
  ): Options {
    const skills =
      msg.connector_skills.length > 0
        ? `\n\nConnector skills installed in this checkout:\n${msg.connector_skills
            .map((s) => `- ${s.name}: ${s.description}`)
            .join("\n")}`
        : "";
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
      for (const pending of [this.questions, this.reviews]) {
        for (const p of pending.values()) p.reject(new Error("Session ended."));
        pending.clear();
      }
      for (const p of this.slotRequests.values()) p.resolve({ approved: false });
      this.slotRequests.clear();
      this.settleSlotMessages("The session ended before the app confirmed delivery.");
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
    const round = this.round;
    if (!round) return;
    if (round.interrupted) return this.endRound("interrupted");
    if (round.terminal === "write_summary" || round.terminal === "record_outcome" || round.terminal === "reply") {
      return this.endRound(round.terminal);
    }
    if (isError) return this.endRound("error");
    // A message the user sent while it worked runs next, and carries the
    // round on: no nudge ahead of it (ADR 0018).
    if (queued > 0) return;
    // No model turn for a slash command: Claude Code ran it by itself
    // (/compact, /context). Nothing to nudge (ADR 0015).
    if (numTurns === 0 && round.nudges === 0 && round.command) return this.endRound("command");
    if (round.nudges < MAX_NUDGES) {
      round.nudges += 1;
      this.push(NUDGE_TEXT);
      return;
    }
    this.endRound("unterminated");
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

  private endRound(kind: TerminalKind): void {
    const round = this.round;
    if (!round) return;
    this.round = undefined;
    // A call let through that never showed up in the stream isn't running.
    for (const id of this.admitted) if (!this.branches.get(id)) this.admitted.delete(id);
    this.heldBack.clear();
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

  async presentReview(input: PresentReviewInput, call: CallContext): Promise<ToolOutcome> {
    if ((await this.attribute(call, input, "present_review")).inSubagent) {
      return { ok: false, error: SUBAGENT_TOOL_ERROR };
    }
    const rejected = this.claimTerminal("present_review");
    if (rejected) return { ok: false, error: rejected };
    const reviewId = randomUUID();
    const decisions = new Promise<string>((resolve, reject) => this.reviews.set(reviewId, { resolve, reject }));
    this.emitter.emit({
      type: "node.review",
      review_id: reviewId,
      findings: input.findings,
      ...(input.summary ? { summary: input.summary } : {}),
    });
    try {
      const text = await decisions;
      return { ok: true, text: `The user's decisions on your findings:\n${text}` };
    } catch (e) {
      return { ok: false, error: (e as Error).message };
    } finally {
      // Like an answer, the decisions resume the round; it still needs its own terminal call.
      if (this.round?.terminal === "present_review") this.round.terminal = null;
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

  async reply(input: ReplyInput, call: CallContext): Promise<ToolOutcome> {
    if ((await this.attribute(call, input, "reply")).inSubagent) {
      return { ok: false, error: SUBAGENT_TOOL_ERROR };
    }
    const rejected = this.claimTerminal("reply");
    if (rejected) return { ok: false, error: rejected };
    this.emitter.emit({ type: "node.reply", message: input.message });
    return { ok: true, text: "Reply shown to the user. End your turn now." };
  }

  async recordOutcome(input: RecordOutcomeInput, call: CallContext): Promise<ToolOutcome> {
    if ((await this.attribute(call, input, "record_outcome")).inSubagent) {
      return { ok: false, error: SUBAGENT_TOOL_ERROR };
    }
    let ref = input.ref;
    let title: string | undefined;
    if (input.kind === "claude_artifact") {
      const link = artifactLink(ref) ?? (this.published.has(ref.trim()) ? ref.trim() : undefined);
      if (!link) return { ok: false, error: ARTIFACT_REF_ERROR };
      ref = link;
      title = this.published.get(link);
    }
    const rejected = this.claimTerminal("record_outcome");
    if (rejected) return { ok: false, error: rejected };
    this.emitter.emit({
      type: "node.outcome",
      kind: input.kind,
      ref,
      summary: input.summary,
      follow_ups: this.followUps(input.follow_ups),
      ...(title ? { title } : {}),
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

  async messageSlot(input: MessageSlotInput, call: CallContext): Promise<ToolOutcome> {
    if ((await this.attribute(call, input, "message_slot")).inSubagent) {
      return { ok: false, error: SUBAGENT_TOOL_ERROR };
    }
    const requestId = randomUUID();
    const routed = new Promise<ToolOutcome>((resolve, reject) => this.slotMessages.set(requestId, { resolve, reject }));
    this.emitter.emit({ type: "slot.message", request_id: requestId, to: input.to, message: input.message });
    return routed;
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
