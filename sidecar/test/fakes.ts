// Test doubles: a scriptable SDK query and a recording emitter.

import type { ModelInfo, Options, SDKMessage, SDKUserMessage } from "@anthropic-ai/claude-agent-sdk";

import type { Emitter } from "../src/ndjson.js";
import type { SidecarToApp } from "../src/protocol.js";
import { AsyncQueue } from "../src/queue.js";
import type { AgentQuery, QueryFn } from "../src/session.js";

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

export class FakeQuery implements AgentQuery {
  readonly stream = new AsyncQueue<SDKMessage>();
  readonly prompts: string[] = [];
  interrupts = 0;
  closed = false;
  options?: Options;
  /** `setModel` and `applyFlagSettings` calls, in order. */
  readonly configured: Array<{ model?: string } | { effortLevel?: string | null }> = [];
  models: ModelInfo[] = [
    {
      value: "opus",
      displayName: "Opus",
      description: "Most capable",
      supportsEffort: true,
      supportedEffortLevels: ["low", "medium", "high", "xhigh", "max"],
    },
    { value: "haiku", displayName: "Haiku", description: "Fastest" },
  ];

  constructor(prompt: AsyncIterable<SDKUserMessage>) {
    void (async () => {
      for await (const m of prompt) {
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

  [Symbol.asyncIterator](): AsyncIterator<SDKMessage> {
    return this.stream[Symbol.asyncIterator]();
  }
}

export function fakeQueryFn(): { fn: QueryFn; last: () => FakeQuery } {
  let last: FakeQuery | undefined;
  return {
    fn: ({ prompt, options }) => {
      last = new FakeQuery(prompt);
      last.options = options;
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
  result: (totalCostUsd: number, subtype = "success") => ({
    type: "result",
    subtype,
    is_error: subtype !== "success",
    total_cost_usd: totalCostUsd,
    duration_ms: 1,
    session_id: "sess-1",
    uuid: "u",
  }),
};
