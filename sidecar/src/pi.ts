// Pi under an OpenAI-compatible provider (ADR 0033): the protocol `Session`
// speaks for Claude Code, on Pi's SDK. Berth's tools are Pi custom tools and
// the round rules are the shared ones (host.ts); Pi's messages reach the app
// in the SDK's envelope (pi-events.ts). Pi keeps its sessions in Berth's own
// directory, apart from the user's own Pi. Its subagents are Berth's `agent`
// tool, a child session each (pi-subagents.ts, ADR 0034).

import { readdirSync, readFileSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

import type {
  AgentSession,
  AgentSessionEvent,
  AuthStorage,
  ModelRegistry,
  SessionManager,
  ToolDefinition,
} from "@mariozechner/pi-coding-agent";
import { z } from "zod";

import { BerthHost, SLASH_COMMAND, type Lane } from "./host.js";
import type { Logger } from "./log.js";
import type { Emitter } from "./ndjson.js";
import { assistantId, lastText, messageEvent, resultEvent, type PiMessage } from "./pi-events.js";
import { loadPi, takeApiKey, type PiModules, type TypeBox } from "./pi-load.js";
import {
  AGENT_TOOL_DESCRIPTION,
  AGENT_TOOL_SCHEMA,
  PI_AGENT_TOOL,
  PiSubagents,
  type AgentInput,
} from "./pi-subagents.js";
import { berthSystemPrompt, piSubagentPrompt, skillsPrompt } from "./prompt.js";
import {
  TOOLS,
  type Effort,
  type JsonValue,
  type PiProvider,
  type SessionResume,
  type SessionStart,
  type ToolName,
  type TurnAttachment,
} from "./protocol.js";
import { callTool, offeredTools, shapes, type CallContext } from "./tools.js";

/** The provider's name in Pi's model registry. */
const PROVIDER = "berth";
/** Pi's thinking levels (pi-agent-core's `ThinkingLevel`). */
type ThinkingLevel = AgentSession["thinkingLevel"];
/** A model's context window when Berth knows none. */
const DEFAULT_WINDOW = 128_000;
/** The most a response may write: Pi asks for it with every request. */
const MAX_OUTPUT = 32_000;
/** Largest attached image sent as an image (larger ones go by path only). */
const MAX_IMAGE_BYTES = 5 * 1024 * 1024;
const IMAGE_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp"];
/** `/compact [instructions]`: the one slash command Pi runs for Berth. */
const COMPACT = /^\/compact(?:\s+([\s\S]*))?$/;

/** Where Pi keeps its sessions when the app doesn't say: Berth's own. */
export function defaultAgentDir(): string {
  return process.env.BERTH_PI_AGENT_DIR || join(homedir(), "Library", "Application Support", "Berth", "pi-agent");
}

/** Berth's effort as Pi's thinking level; none leaves it to the model. */
export function thinkingLevel(effort: Effort | null | undefined): ThinkingLevel {
  switch (effort) {
    case "low":
    case "medium":
    case "high":
    case "xhigh":
      return effort;
    case "max":
      return "xhigh";
    default:
      return "off";
  }
}

/** A berth tool's input schema as JSON Schema, for Pi. */
function inputSchema(name: ToolName): object {
  const schema: Record<string, unknown> = { ...z.toJSONSchema(z.object(shapes[name]), { io: "input" }) };
  delete schema.$schema;
  return schema;
}

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export interface PiSessionDeps {
  emitter: Emitter;
  log: Logger;
  now?: () => number;
  /** Pi's SDK; tests may hand one in. */
  load?: () => Promise<PiModules>;
  /** Where Pi keeps its sessions (`sessions/` in it). */
  agentDir?: string;
}

export class PiSession extends BerthHost {
  private readonly load: () => Promise<PiModules>;
  private readonly agentDir: string;
  private ready?: Promise<boolean>;
  private session?: AgentSession;
  private modules?: PiModules;
  private auth?: AuthStorage;
  private registry?: ModelRegistry;
  private subagents?: PiSubagents;
  /** What a subagent's prompt gets of the session's: the slot, the skills. */
  private context = "";
  private provider?: PiProvider;
  private startModel?: string;
  private effort: Effort | null = null;
  private stopped = false;
  private finish!: () => void;
  private readonly over = new Promise<void>((resolve) => (this.finish = resolve));
  private seq = 0;
  /** A prompt (or `/compact`) runs; a turn sent meanwhile steers it. */
  private running = false;
  /** In the run going on: assistant messages, and the error it ended on. */
  private turns = 0;
  private failure?: string;
  /** Messages Pi holds to steer the run with. */
  private queued = 0;

  constructor(deps: PiSessionDeps) {
    super(deps.emitter, deps.log, deps.now ?? Date.now);
    this.load = deps.load ?? loadPi;
    this.agentDir = deps.agentDir ?? defaultAgentDir();
  }

  get started(): boolean {
    return this.ready !== undefined;
  }

  /** Resolves when the session stops. */
  get done(): Promise<void> {
    return this.over;
  }

  // ---- app -> sidecar ----

  start(msg: SessionStart | SessionResume, replyTo: string): void {
    if (this.ready) {
      this.error("session already started", false, replyTo);
      return;
    }
    this.cwd = msg.cwd;
    this.ready = this.open(msg, replyTo).catch((e: unknown) => {
      this.error(`Pi could not start: ${message(e)}`, true, replyTo);
      this.finish();
      return false;
    });
  }

  private async open(msg: SessionStart | SessionResume, replyTo: string): Promise<boolean> {
    if (!msg.pi) throw new Error("no OpenAI-compatible API to run on");
    this.modules = await this.load();
    const { pi, Type } = this.modules;
    this.provider = msg.pi;
    this.startModel = msg.model ?? msg.pi.models[0]?.id;
    this.effort = msg.effort ?? null;
    // Pi's tools run commands in this process's environment: the agent's.
    Object.assign(process.env, msg.env);

    const auth = pi.AuthStorage.inMemory();
    // Given here, the key is never read as a command or a variable's name,
    // as a provider's configured key would be.
    const key = takeApiKey();
    if (key) auth.setRuntimeApiKey(PROVIDER, key);
    this.auth = auth;
    this.registry = pi.ModelRegistry.inMemory(auth);
    this.register();
    const model = this.startModel ? this.registry.find(PROVIDER, this.startModel) : undefined;
    if (!model) throw new Error(this.startModel ? `${this.startModel} isn't on offer` : "no model is on offer");

    const sessionDir = join(this.agentDir, "sessions");
    const resume = "claude_session_id" in msg ? msg.claude_session_id : undefined;
    const sessionManager = resume
      ? this.reopen(pi.SessionManager, resume, sessionDir, msg.cwd, "resume_at" in msg ? msg.resume_at : undefined)
      : pi.SessionManager.create(msg.cwd, sessionDir);
    const slotMessages = msg.slot_messages !== false;
    const maxSubagents = msg.max_subagents;
    this.subagents = new PiSubagents(
      {
        emitter: this.emitter,
        now: this.now,
        cwd: () => this.cwd,
        sessionId: () => this.session?.sessionId,
        raw: (kind, json, branchId) => this.raw(kind, json, branchId),
        spent: (usd) => (this.totalUsd += usd),
      },
      maxSubagents,
    );
    this.context = [skillsPrompt(msg.connector_skills).trim(), msg.system_prompt]
      .filter((s) => s.trim().length > 0)
      .join("\n\n");
    const loader = await this.loader(
      [berthSystemPrompt({ slotMessages, maxSubagents, pi: true }) + skillsPrompt(msg.connector_skills), msg.system_prompt]
        .filter((s) => s.trim().length > 0)
        .join("\n\n"),
    );
    const { session } = await pi.createAgentSession({
      cwd: msg.cwd,
      agentDir: this.agentDir,
      authStorage: auth,
      modelRegistry: this.registry,
      model,
      thinkingLevel: thinkingLevel(this.effort),
      customTools: this.tools(Type, slotMessages, maxSubagents),
      resourceLoader: loader,
      sessionManager,
      settingsManager: pi.SettingsManager.inMemory({}),
    });
    if (this.stopped) {
      session.dispose();
      return false;
    }
    this.session = session;
    if (msg.title && !resume) session.setSessionName(msg.title);
    session.subscribe((e) => this.onEvent(e));
    this.raw("system", {
      type: "system",
      subtype: "init",
      agent: "pi",
      session_id: session.sessionId,
      cwd: msg.cwd,
      model: model.id,
    });
    this.emitter.emit({ type: "session.started", claude_session_id: session.sessionId }, replyTo);
    this.emitter.emit({
      type: "session.commands",
      commands: [
        {
          name: "compact",
          description: "Summarize the conversation so far, to free the context window",
          argument_hint: "[instructions]",
          aliases: [],
          builtin: true,
        },
      ],
      terminal: [],
    });
    this.reportContext();
    return true;
  }

  /** A saved session, by its id, for this checkout; at `resumeAt`, the
   * entry a rewind goes back to, Pi branches its tree (ADR 0009). */
  private reopen(
    sessions: typeof SessionManager,
    id: string,
    sessionDir: string,
    cwd: string,
    resumeAt: string | undefined,
  ): SessionManager {
    let file: string | undefined;
    try {
      file = readdirSync(sessionDir).find((f) => f.endsWith(`_${id}.jsonl`));
    } catch {
      file = undefined;
    }
    if (!file) throw new Error(`its conversation (${id}) is gone from ${sessionDir}`);
    const sm = sessions.open(join(sessionDir, file), sessionDir, cwd);
    if (resumeAt) sm.branch(resumeAt);
    return sm;
  }

  /** The provider's models in Pi's registry; a model reasons only when the
   * slot asks for an effort (a model that doesn't would refuse it). */
  private register(): void {
    const p = this.provider;
    if (!p || !this.registry) return;
    const zero = { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 };
    this.registry.registerProvider(PROVIDER, {
      baseUrl: p.base_url.replace(/\/+$/, ""),
      api: p.api === "chat" ? "openai-completions" : "openai-responses",
      // The key is the auth storage's (above); Pi needs one here regardless.
      apiKey: "none",
      models: p.models.map((m) => {
        const window = m.context_window ?? DEFAULT_WINDOW;
        return {
          id: m.id,
          name: m.id,
          reasoning: this.effort !== null,
          input: ["text", "image"] as ("text" | "image")[],
          cost: zero,
          contextWindow: window,
          maxTokens: Math.max(1024, Math.min(MAX_OUTPUT, Math.floor(window / 4))),
        };
      }),
    });
  }

  /** Pi's resources for a session: Berth's prompt after Pi's, the
   * checkout's context files and skills, and nothing of the user's own Pi. */
  private async loader(prompt: string) {
    const pi = this.modules?.pi;
    if (!pi) throw new Error("Pi isn't loaded");
    const loader = new pi.DefaultResourceLoader({
      cwd: this.cwd,
      agentDir: this.agentDir,
      noExtensions: true,
      noPromptTemplates: true,
      noThemes: true,
      // Connector skills go where Claude Code reads them (§8.2).
      additionalSkillPaths: [join(this.cwd, ".claude", "skills")],
      appendSystemPrompt: [prompt],
    });
    await loader.reload();
    return loader;
  }

  private tools(Type: TypeBox, slotMessages: boolean, maxSubagents: number | undefined): ToolDefinition[] {
    const berth: ToolDefinition[] = offeredTools({ slotMessages }).map((name) => this.berthTool(Type, name));
    if (maxSubagents === 0) return berth;
    return [
      ...berth,
      {
        name: PI_AGENT_TOOL,
        label: "Agent",
        description: AGENT_TOOL_DESCRIPTION,
        parameters: Type.Unsafe(AGENT_TOOL_SCHEMA),
        execute: async (toolCallId: string, params: unknown, signal?: AbortSignal) => {
          const text = await this.subagents?.run(toolCallId, params as AgentInput, signal, (branchId) =>
            this.openChild(branchId),
          );
          return { content: [{ type: "text" as const, text: text ?? "" }], details: {} };
        },
      },
    ];
  }

  /** A berth tool as Pi's; `branchId`: a subagent's, on its lane. */
  private berthTool(Type: TypeBox, name: ToolName, branchId?: string): ToolDefinition {
    return {
      name,
      label: name,
      description: TOOLS[name].description,
      parameters: Type.Unsafe(inputSchema(name)),
      execute: async (toolCallId: string, params: unknown) => {
        if (branchId) this.subagents?.bind(toolCallId, branchId);
        const out = await callTool(this, name, params, { toolUseId: toolCallId });
        if (!out.ok) throw new Error(out.error);
        return { content: [{ type: "text" as const, text: out.text }], details: {} };
      },
    };
  }

  /** A subagent's own session (ADR 0034): this checkout, provider and
   * model; Pi's tools and \`record_decision\`; kept in memory only. */
  private async openChild(branchId: string): Promise<AgentSession> {
    const { modules, auth, registry, session } = this;
    if (!modules || !auth || !registry || !session?.model) throw new Error("The session isn't running.");
    const { pi, Type } = modules;
    const { session: child } = await pi.createAgentSession({
      cwd: this.cwd,
      agentDir: this.agentDir,
      authStorage: auth,
      modelRegistry: registry,
      model: session.model,
      thinkingLevel: session.thinkingLevel,
      customTools: [this.berthTool(Type, "record_decision", branchId)],
      resourceLoader: await this.loader(piSubagentPrompt(this.context)),
      sessionManager: pi.SessionManager.inMemory(this.cwd),
      settingsManager: pi.SettingsManager.inMemory({}),
    });
    return child;
  }

  turn(text: string, attachments: TurnAttachment[] = []): void {
    if (!this.ready) {
      this.error("session.turn before session.start", false);
      return;
    }
    void this.ready.then((ok) => {
      if (ok) this.take(text, attachments);
    });
  }

  private take(text: string, attachments: TurnAttachment[]): void {
    const session = this.session;
    if (!session) return;
    const images = attachments.flatMap((a) => this.image(a));
    // Sent while the agent works, it joins the run (ADR 0018).
    if (this.running) {
      session.steer(text, images).catch((e: unknown) => this.error(`Could not pass it on: ${message(e)}`, false));
      return;
    }
    if (!this.round) {
      this.round = this.newRound();
      this.round.command = SLASH_COMMAND.test(text.trimStart());
    }
    const compact = COMPACT.exec(text.trim());
    if (compact) void this.compact(compact[1]);
    else void this.run(text, images);
  }

  /** One prompt to its end: Pi's run, with its retries and compaction. */
  private async run(text: string, images: ReturnType<PiSession["image"]> = []): Promise<void> {
    const session = this.session;
    if (!session) return;
    this.running = true;
    this.turns = 0;
    this.failure = undefined;
    const startedAt = this.now();
    try {
      await session.prompt(text, images.length > 0 ? { images } : undefined);
    } catch (e) {
      this.failure = message(e);
    }
    this.running = false;
    if (this.failure) this.error(this.failure, false);
    this.raw(
      "result",
      resultEvent({
        isError: this.failure !== undefined,
        numTurns: this.turns,
        durationMs: Math.max(0, this.now() - startedAt),
        totalUsd: this.totalUsd,
        sessionId: session.sessionId,
        ...(this.failure ? { error: this.failure } : {}),
      }),
    );
    this.emitter.emit({ type: "session.cost", total_usd: this.totalUsd });
    this.reportContext();
    if (this.stopped) return;
    this.settleRound(this.failure !== undefined, this.turns, this.queued);
  }

  /** `/compact`: Pi summarizes the conversation; the round is the command alone. */
  private async compact(instructions?: string): Promise<void> {
    const session = this.session;
    if (!session) return;
    this.running = true;
    try {
      await session.compact(instructions?.trim() || undefined);
    } catch (e) {
      this.error(`Could not compact: ${message(e)}`, false);
    }
    this.running = false;
    if (this.round) this.round.lastText = "Compacted the conversation.";
    if (!this.stopped) this.settleRound(false, 0, 0);
  }

  protected nudge(text: string): void {
    void this.run(text);
  }

  /** A subagent's `record_decision` is on its branch (ADR 0034). */
  protected lane(call: CallContext): Promise<Lane> {
    const branchId = this.subagents?.laneOf(call.toolUseId);
    return Promise.resolve(branchId ? { branchId, inSubagent: true } : { inSubagent: false });
  }

  async configure(model: string | null, effort: Effort | null): Promise<void> {
    if (!(await this.ready) || !this.session || !this.registry) {
      this.error("session.configure before session.start", false);
      return;
    }
    try {
      this.effort = effort;
      this.register();
      const id = model ?? this.startModel;
      const m = id ? this.registry.find(PROVIDER, id) : undefined;
      if (!m) throw new Error(`${id ?? "no model"} isn't on offer`);
      await this.session.setModel(m);
      this.session.setThinkingLevel(thinkingLevel(effort));
    } catch (e) {
      this.error(`Could not switch the model: ${message(e)}`, false);
    }
  }

  async interrupt(): Promise<void> {
    if (!this.session || !this.round) return;
    this.round.interrupted = true;
    this.abandonPending("Interrupted by the user.", "Interrupted by the user before the app confirmed delivery.");
    try {
      await this.session.abort();
    } catch (e) {
      this.log.warn({ err: e }, "abort failed");
    }
  }

  stop(): void {
    if (this.stopped) return;
    this.stopped = true;
    this.abandonPending("Session ended.", "The session ended before the app confirmed delivery.");
    const session = this.session;
    if (!session) {
      if (!this.ready) this.finish();
      else void this.ready.then(() => this.finish());
      return;
    }
    void session
      .abort()
      .catch(() => undefined)
      .finally(() => {
        session.dispose();
        this.finish();
      });
  }

  /** Pi runs nothing in the background (ADR 0033). */
  async background(): Promise<void> {
    this.error("Pi runs nothing in the background.", false);
  }

  /** Its tasks are its subagents: one stops, the round goes on (ADR 0034). */
  async stopTask(taskId: string): Promise<void> {
    if (!this.subagents?.stop(taskId)) this.log.info({ taskId }, "no such subagent running");
  }

  async readTask(taskId: string): Promise<void> {
    this.emitter.emit({
      type: "task.output",
      task_id: taskId,
      output: "",
      total_bytes: 0,
      truncated: false,
      error: "A subagent has no output to read.",
    });
  }

  // ---- Pi's events ----

  /** Exposed for tests; normally Pi's session calls it. */
  onEvent(e: AgentSessionEvent): void {
    switch (e.type) {
      case "message_end":
        this.onMessage(e.message as PiMessage);
        break;
      case "tool_execution_end":
        // After the call's own message went out, as its events come in.
        if (e.toolName === PI_AGENT_TOOL) {
          queueMicrotask(() => this.subagents?.ended(e.toolCallId, e.result, e.isError));
        }
        break;
      case "queue_update":
        this.queued = e.steering.length + e.followUp.length;
        break;
      case "compaction_start":
        this.raw("system", { type: "system", subtype: "status", status: "compacting" });
        break;
      case "compaction_end":
        this.raw("system", { type: "system", subtype: "status", status: null });
        if (e.result) {
          const trigger = e.reason === "manual" ? "manual" : "auto";
          this.raw("system", {
            type: "system",
            subtype: "compact_boundary",
            compact_metadata: { trigger, pre_tokens: e.result.tokensBefore },
          });
          this.emitter.emit({ type: "node.compacted", trigger, pre_tokens: e.result.tokensBefore });
        } else if (e.errorMessage && !e.aborted) {
          this.error(`Compacting failed: ${e.errorMessage}`, false);
        }
        this.reportContext();
        break;
      case "auto_retry_start":
        this.raw("system", {
          type: "system",
          subtype: "api_retry",
          attempt: e.attempt,
          max_retries: e.maxAttempts,
          retry_delay_ms: e.delayMs,
          error_message: e.errorMessage,
        });
        break;
      default:
        break;
    }
  }

  private onMessage(m: PiMessage): void {
    if (m.role === "assistant" && "content" in m) {
      this.turns += 1;
      const a = m as Extract<PiMessage, { role: "assistant" }>;
      this.totalUsd += a.usage?.cost?.total ?? 0;
      if (a.stopReason === "error") this.failure = a.errorMessage ?? "The model's request failed.";
      const text = lastText(m);
      if (text && this.round) this.round.lastText = text;
    }
    // Pi saves the message as soon as its listeners have it: its entry id,
    // the event's `uuid`, is there a moment later.
    queueMicrotask(() => {
      const event = messageEvent(m, this.session?.sessionId, this.session?.sessionManager.getLeafId() ?? undefined);
      if (event) this.raw(event.kind, event.json);
      // The subagents it starts open their branches after it (§5.4).
      if (m.role === "assistant" && "content" in m) {
        const a = m as Extract<PiMessage, { role: "assistant" }>;
        this.subagents?.spawned(assistantId(a), a.content);
      }
    });
  }

  private raw(kind: string, json: { [key: string]: JsonValue }, branchId?: string): void {
    this.emitter.emit({
      type: "raw.event",
      seq: ++this.seq,
      kind,
      json,
      ...(branchId ? { branch_id: branchId } : {}),
    });
  }

  private reportContext(): void {
    const u = this.session?.getContextUsage();
    if (!u) return;
    const used = Math.max(0, Math.round(u.tokens ?? 0));
    const max = u.contextWindow;
    this.emitter.emit({
      type: "session.context",
      categories: [
        { name: "Conversation", tokens: used, kind: "used" },
        { name: "Free space", tokens: Math.max(0, max - used), kind: "free" },
      ],
      max_tokens: max,
    });
  }

  /** An attached image as Pi's image; nothing if unreadable or too large. */
  private image(a: TurnAttachment): Array<{ type: "image"; data: string; mimeType: string }> {
    if (!IMAGE_TYPES.includes(a.mime)) return [];
    try {
      if (statSync(a.path).size > MAX_IMAGE_BYTES) return [];
      return [{ type: "image", data: readFileSync(a.path).toString("base64"), mimeType: a.mime }];
    } catch (e) {
      this.log.warn({ err: e, path: a.path }, "attachment unreadable");
      return [];
    }
  }
}
