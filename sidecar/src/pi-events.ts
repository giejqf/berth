// Pi's messages as raw events in the Claude Agent SDK's envelope (ADR 0033),
// so the app's transcript, live card and rewind read one shape: `assistant`
// with text / thinking / tool_use blocks, `user` with the prompt or
// tool_result blocks, `result` at the end of a run. Berth's tools carry the
// name Claude Code gives them (`mcp__berth__ask_user`); Pi's own (`read`,
// `bash`, `edit`, `write`) keep theirs and their arguments. `uuid` is the
// message's entry in Pi's session tree: where a rewind resumes.

import { TOOLS, type JsonValue, type ToolName } from "./protocol.js";
import { qualifiedToolName } from "./tools.js";

/** Pi's message shapes, as far as Berth reads them (pi-ai's types). */
export type PiContent =
  | { type: "text"; text: string }
  | { type: "thinking"; thinking: string }
  | { type: "image"; data: string; mimeType: string }
  | { type: "toolCall"; id: string; name: string; arguments: Record<string, unknown> };

export interface PiUsage {
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
  cost?: { total: number };
}

export type PiMessage =
  | { role: "user"; content: string | PiContent[]; timestamp?: number }
  | {
      role: "assistant";
      content: PiContent[];
      model?: string;
      responseId?: string;
      usage?: PiUsage;
      stopReason?: "stop" | "length" | "toolUse" | "error" | "aborted";
      errorMessage?: string;
      timestamp?: number;
    }
  | {
      role: "toolResult";
      toolCallId: string;
      toolName: string;
      content: PiContent[];
      isError: boolean;
      timestamp?: number;
    }
  | { role: string };

type Json = { [key: string]: JsonValue };

function block(c: PiContent): Json | undefined {
  switch (c.type) {
    case "text":
      return { type: "text", text: c.text };
    case "thinking":
      return { type: "thinking", thinking: c.thinking };
    case "image":
      return { type: "image", source: { type: "base64", media_type: c.mimeType, data: c.data } };
    case "toolCall":
      return {
        type: "tool_use",
        id: c.id,
        name: c.name in TOOLS ? qualifiedToolName(c.name as ToolName) : c.name,
        input: c.arguments as JsonValue,
      };
  }
}

function blocks(content: PiContent[]): Json[] {
  return content.map(block).filter((b): b is Json => b !== undefined);
}

const STOP: Record<string, string> = { stop: "end_turn", toolUse: "tool_use", length: "max_tokens" };

/** The event for a Pi message; undefined for what isn't a conversation
 * message (compaction summaries, custom entries). */
export function messageEvent(
  m: PiMessage,
  sessionId: string | undefined,
  uuid: string | undefined,
): { kind: "assistant" | "user"; json: Json } | undefined {
  const common: Json = {
    parent_tool_use_id: null,
    ...(sessionId ? { session_id: sessionId } : {}),
    ...(uuid ? { uuid } : {}),
  };
  if (m.role === "assistant" && "content" in m) {
    const a = m as Extract<PiMessage, { role: "assistant" }>;
    const u = a.usage;
    return {
      kind: "assistant",
      json: {
        type: "assistant",
        message: {
          id: a.responseId ?? `pi-${a.timestamp ?? 0}`,
          type: "message",
          role: "assistant",
          model: a.model ?? "",
          content: blocks(a.content),
          stop_reason: STOP[a.stopReason ?? ""] ?? null,
          ...(u
            ? {
                usage: {
                  input_tokens: u.input,
                  output_tokens: u.output,
                  cache_read_input_tokens: u.cacheRead,
                  cache_creation_input_tokens: u.cacheWrite,
                },
              }
            : {}),
        },
        ...(a.errorMessage ? { error: a.errorMessage } : {}),
        ...common,
      },
    };
  }
  if (m.role === "toolResult" && "toolCallId" in m) {
    const r = m as Extract<PiMessage, { role: "toolResult" }>;
    return {
      kind: "user",
      json: {
        type: "user",
        message: {
          role: "user",
          content: [{ type: "tool_result", tool_use_id: r.toolCallId, content: blocks(r.content), is_error: r.isError }],
        },
        ...common,
      },
    };
  }
  if (m.role === "user" && "content" in m) {
    const c = (m as Extract<PiMessage, { role: "user" }>).content;
    return {
      kind: "user",
      json: { type: "user", message: { role: "user", content: typeof c === "string" ? c : blocks(c) }, ...common },
    };
  }
  return undefined;
}

/** The end of a run (Pi's `agent_end`), as the SDK's `result`. */
export function resultEvent(args: {
  isError: boolean;
  numTurns: number;
  durationMs: number;
  totalUsd: number;
  sessionId?: string;
  error?: string;
}): Json {
  return {
    type: "result",
    subtype: args.isError ? "error_during_execution" : "success",
    is_error: args.isError,
    num_turns: args.numTurns,
    duration_ms: args.durationMs,
    total_cost_usd: args.totalUsd,
    ...(args.error ? { result: args.error } : {}),
    ...(args.sessionId ? { session_id: args.sessionId } : {}),
  };
}

/** The text an assistant message ends on, for the round's last line. */
export function lastText(m: PiMessage): string | undefined {
  if (m.role !== "assistant" || !("content" in m)) return undefined;
  const texts = (m.content as PiContent[]).filter((c) => c.type === "text").map((c) => (c as { text: string }).text);
  const text = texts.join("\n").trim();
  return text || undefined;
}
