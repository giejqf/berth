// Subagent branch tracking (AGENTS.md §5.4). Attribution comes from the stream:
// an `Agent` tool_use opens a branch; events carrying its id as
// `parent_tool_use_id` belong to it; its tool_result (or, for background agents,
// the task_notification) closes it.

import { randomUUID } from "node:crypto";

import type { BranchCollected, BranchSpawned, BranchStatus } from "./protocol.js";

/** Tool names that spawn subagents ("Task" is the pre-rename name). */
export const AGENT_TOOLS = new Set(["Agent", "Task"]);
/** Tools whose `file_path` input marks a file as touched. */
const EDIT_TOOLS = new Set(["Edit", "Write", "MultiEdit", "NotebookEdit"]);

export interface Branch {
  branchId: string;
  toolUseId: string;
  parentBranchId?: string;
  spawnedAt: number;
  toolCalls: number;
  touchedFiles: Set<string>;
  background: boolean;
  collected: boolean;
}

export class BranchTracker {
  private readonly byToolUseId = new Map<string, Branch>();

  constructor(private readonly now: () => number = Date.now) {}

  /** Branch whose Agent tool_use has this id. */
  get(toolUseId: string): Branch | undefined {
    return this.byToolUseId.get(toolUseId);
  }

  open(): Branch[] {
    return [...this.byToolUseId.values()].filter((b) => !b.collected);
  }

  spawn(args: {
    toolUseId: string;
    input: unknown;
    parentBranchId?: string;
    parallelGroup?: string;
  }): BranchSpawned {
    const input = (typeof args.input === "object" && args.input !== null ? args.input : {}) as Record<string, unknown>;
    const branch: Branch = {
      branchId: randomUUID(),
      toolUseId: args.toolUseId,
      parentBranchId: args.parentBranchId,
      spawnedAt: this.now(),
      toolCalls: 0,
      touchedFiles: new Set(),
      background: false,
      collected: false,
    };
    this.byToolUseId.set(args.toolUseId, branch);
    const str = (k: string) => (typeof input[k] === "string" ? (input[k] as string) : undefined);
    return {
      branch_id: branch.branchId,
      parent_tool_use_id: args.toolUseId,
      ...(args.parentBranchId ? { parent_branch_id: args.parentBranchId } : {}),
      agent_name: str("subagent_type") ?? str("name") ?? "general-purpose",
      description: str("description") ?? "",
      prompt: str("prompt") ?? "",
      ...(args.parallelGroup ? { parallel_group: args.parallelGroup } : {}),
    };
  }

  /** Record a tool call made inside `branch`. */
  recordToolUse(branch: Branch, name: string, input: unknown): void {
    branch.toolCalls += 1;
    if (!EDIT_TOOLS.has(name) || typeof input !== "object" || input === null) return;
    const i = input as Record<string, unknown>;
    const path = i.file_path ?? i.notebook_path;
    if (typeof path === "string" && path.length > 0) branch.touchedFiles.add(path);
  }

  collect(
    branch: Branch,
    args: { resultText: string; status: BranchStatus; durationMs?: number; toolCalls?: number },
  ): BranchCollected {
    branch.collected = true;
    return {
      branch_id: branch.branchId,
      result_text: args.resultText,
      status: args.status,
      duration_ms: args.durationMs ?? Math.max(0, this.now() - branch.spawnedAt),
      tool_calls: args.toolCalls ?? branch.toolCalls,
      touched_files: [...branch.touchedFiles].sort(),
    };
  }
}

/** Extract an Agent tool's result from a tool_result block + `tool_use_result`. */
export function agentResult(
  block: { content?: unknown; is_error?: boolean },
  toolUseResult: unknown,
): { kind: "background" } | { kind: "done"; text: string; isError: boolean; durationMs?: number; toolCalls?: number } {
  const r = (typeof toolUseResult === "object" && toolUseResult !== null ? toolUseResult : {}) as Record<
    string,
    unknown
  >;
  if (r.status === "async_launched" || r.status === "remote_launched") return { kind: "background" };
  const text = textOf(r.content) || textOf(block.content);
  return {
    kind: "done",
    text,
    isError: block.is_error === true,
    durationMs: typeof r.totalDurationMs === "number" ? r.totalDurationMs : undefined,
    toolCalls: typeof r.totalToolUseCount === "number" ? r.totalToolUseCount : undefined,
  };
}

/** Concatenate text from a string or an array of `{type:"text", text}` blocks. */
export function textOf(content: unknown): string {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content
    .map((b: unknown) =>
      typeof b === "object" && b !== null && (b as { type?: unknown }).type === "text"
        ? String((b as { text?: unknown }).text ?? "")
        : "",
    )
    .filter((t) => t.length > 0)
    .join("\n");
}

export function taskStatus(status: string): BranchStatus {
  switch (status) {
    case "completed":
      return "ok";
    case "failed":
      return "error";
    default:
      return "cancelled";
  }
}
