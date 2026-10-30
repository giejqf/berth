// Test doubles: a scriptable SDK query and a recording emitter.

import type { AccountInfo, ModelInfo, Options, SDKMessage, SDKUserMessage, SlashCommand } from "@anthropic-ai/claude-agent-sdk";

import type { Emitter } from "../src/ndjson.js";
import type { SidecarToApp } from "../src/protocol.js";
import { AsyncQueue } from "../src/queue.js";
import type { AgentQuery, QueryFn } from "../src/session.js";
import type { ContextReport, UsageReport } from "../src/usage.js";

export class RecordingEmitter implements Emitter {
  readonly sent: Array<{ body: SidecarToApp; replyTo?: string }> = [];

  emit(body: SidecarToApp, replyTo?: string): void {
    this.sent.push({ body, replyTo });
  }

  ofType<T extends SidecarToApp["type"]>(type: T): Array<Extract<SidecarToApp, { type: T }>> {
    return this.sent.map((s) => s.body).filter((b): b is Extract<SidecarToApp, { type: T }> => b.type === type);
  }

  types(): string[] {
    return this.sent.map((s) => s.body.type).filter((t) => t !== "raw.event");
  }
}

/** What the fake's usage call answers: a Max plan, extra usage off. */
export const USAGE_REPORT: UsageReport = {
  subscription_type: "max",
  rate_limits_available: true,
  rate_limits: {
    five_hour: { utilization: 23, resets_at: "2026-10-20T15:40:00Z" },
    seven_day: { utilization: 41, resets_at: null },
    model_scoped: [{ display_name: "Opus", utilization: 12, resets_at: null }],
    extra_usage: { is_enabled: false, monthly_limit: null, used_credits: null, utilization: null },
  },
};

export class FakeQuery implements AgentQuery {
  readonly stream = new AsyncQueue<SDKMessage>();
  readonly prompts: string[] = [];
  /** The user messages as sent, uuids included. */
  readonly messages: SDKUserMessage[] = [];
  interrupts = 0;
  closed = false;
  options?: Options;
  /** `setModel` and `applyFlagSettings` calls, in order. */
  readonly configured: Array<{ model?: string } | { effortLevel?: string | null }> = [];
  models: ModelInfo[] = [
    { value: "default", resolvedModel: "claude-opus-5-5", displayName: "Default", description: "Recommended" },
    {
      value: "opus",
      resolvedModel: "claude-opus-5-5",
      displayName: "Opus",
      description: "Most capable",
      supportsEffort: true,
      supportedEffortLevels: ["low", "medium", "high", "xhigh", "max"],
    },
    {
      value: "claude-opus-5-5",
      resolvedModel: "claude-opus-5-5",
      displayName: "Opus 5.5",
      description: "Same model by id",
    },
    { value: "claude-opus-4-8", displayName: "Opus 4.8", description: "Previous Opus" },
    { value: "haiku", displayName: "Haiku", description: "Fastest" },
  ];

  constructor(prompt: AsyncIterable<SDKUserMessage>) {
    void (async () => {
      for await (const m of prompt) {
        this.messages.push(m);
        const content = m.message.content;
        this.prompts.push(typeof content === "string" ? content : JSON.stringify(content));
      }
    })();
  }

  send(msg: object): void {
    this.stream.push(msg as SDKMessage);
  }

  interrupt(): Promise<unknown> {
    this.interrupts += 1;
    return Promise.resolve(undefined);
  }

  close(): void {
    this.closed = true;
    this.stream.end();
  }

  setModel(model?: string): Promise<void> {
    this.configured.push({ model });
    return Promise.resolve();
  }

  applyFlagSettings(settings: { effortLevel?: string | null }): Promise<void> {
    this.configured.push({ effortLevel: settings.effortLevel });
    return Promise.resolve();
  }

  supportedModels(): Promise<ModelInfo[]> {
    return Promise.resolve(this.models);
  }

  commands: SlashCommand[] = [
    { name: "compact", description: "Compact the conversation", argumentHint: "<instructions>", builtin: true },
    { name: "usage", description: "Plan usage", argumentHint: "", aliases: ["cost"], builtin: true },
    { name: "code-review", description: "Review the diff", argumentHint: "[focus]" },
  ];

  supportedCommands(): Promise<SlashCommand[]> {
    return Promise.resolve(this.commands);
  }

  contextCalls = 0;
  context: ContextReport = {
    categories: [
      { name: "Messages", tokens: 40_000, color: "", kind: "used" },
      { name: "Free space", tokens: 127_000, color: "", kind: "free" },
      { name: "Autocompact buffer", tokens: 33_000, color: "", kind: "buffer" },
    ],
  };

  getContextUsage(): Promise<ContextReport> {
    this.contextCalls += 1;
    return Promise.resolve(this.context);
  }

  usageCalls = 0;
  usage: UsageReport = USAGE_REPORT;

  account: AccountInfo = { email: "dev@example.com", subscriptionType: "Claude Max", apiProvider: "firstParty" };

  accountInfo(): Promise<AccountInfo> {
    return Promise.resolve(this.account);
  }

  backgrounded: Array<string | undefined> = [];

  backgroundTasks(toolUseId?: string): Promise<boolean> {
    this.backgrounded.push(toolUseId);
    return Promise.resolve(true);
  }

  stopped: string[] = [];

  stopTask(taskId: string): Promise<void> {
    this.stopped.push(taskId);
    return Promise.resolve();
  }

  taskOutput: Record<string, string> = {};

  getTaskOutput(taskId: string): Promise<{ output: string; total_bytes: number; truncated: boolean }> {
    const output = this.taskOutput[taskId];
    if (output === undefined) return Promise.reject(new Error(`no such task: ${taskId}`));
    return Promise.resolve({ output, total_bytes: output.length, truncated: false });
  }

  usage_EXPERIMENTAL_MAY_CHANGE_DO_NOT_RELY_ON_THIS_API_YET(): Promise<UsageReport> {
    this.usageCalls += 1;
    return Promise.resolve(this.usage);
  }

  [Symbol.asyncIterator](): AsyncIterator<SDKMessage> {
    return this.stream[Symbol.asyncIterator]();
  }
}

export function fakeQueryFn(): { fn: QueryFn; last: () => FakeQuery; all: FakeQuery[] } {
  let last: FakeQuery | undefined;
  const all: FakeQuery[] = [];
  return {
    all,
    fn: ({ prompt, options }) => {
      last = new FakeQuery(prompt);
      last.options = options;
      all.push(last);
      return last;
    },
    last: () => {
      if (!last) throw new Error("query not started");
      return last;
    },
  };
}

/** Let queued stream messages and promise continuations run. */
export async function flush(): Promise<void> {
  for (let i = 0; i < 10; i++) await new Promise((r) => setImmediate(r));
}

export const sdk = {
  init: (sessionId = "sess-1") => ({ type: "system", subtype: "init", session_id: sessionId, uuid: "u" }),
  text: (text: string, parent: string | null = null, id = "msg-1") => ({
    type: "assistant",
    parent_tool_use_id: parent,
    uuid: "u",
    session_id: "sess-1",
    message: { id, role: "assistant", content: [{ type: "text", text }] },
  }),
  toolUse: (toolId: string, name: string, input: unknown, parent: string | null = null, id = "msg-1") => ({
    type: "assistant",
    parent_tool_use_id: parent,
    uuid: "u",
    session_id: "sess-1",
    message: { id, role: "assistant", content: [{ type: "tool_use", id: toolId, name, input }] },
  }),
  toolResult: (toolId: string, content: unknown, toolUseResult?: unknown, isError = false) => ({
    type: "user",
    parent_tool_use_id: null,
    session_id: "sess-1",
    tool_use_result: toolUseResult,
    message: { role: "user", content: [{ type: "tool_result", tool_use_id: toolId, content, is_error: isError }] },
  }),
  result: (totalCostUsd: number, subtype = "success", numTurns = 1, queuedTurnCount = 0) => ({
    type: "result",
    subtype,
    queued_turn_count: queuedTurnCount,
    is_error: subtype !== "success",
    ...(subtype !== "success" ? { errors: [] as string[] } : {}),
    num_turns: numTurns,
    total_cost_usd: totalCostUsd,
    duration_ms: 1,
    session_id: "sess-1",
    uuid: "u",
  }),
};
