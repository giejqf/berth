// The "berth" MCP server: app-owned agent tools (AGENTS.md §5.2).
//
// Schemas mirror the generated protocol types; the assignability checks below
// fail to compile when crates/berth-protocol changes and this file does not.

import { createSdkMcpServer, tool, type McpSdkServerConfigWithInstance } from "@anthropic-ai/claude-agent-sdk";
import { z } from "zod";

import {
  MCP_SERVER_NAME,
  TOOLS,
  type AskUserInput,
  type MessageSlotInput,
  type PresentArtifactInput,
  type PresentReviewInput,
  type RecordDecisionInput,
  type RecordOutcomeInput,
  type ReplyInput,
  type RequestSlotInput,
  type ToolName,
  type WriteSummaryInput,
} from "./protocol.js";
import { VERSION } from "./version.js";

const followUp = z.object({
  id: z.string().optional(),
  title: z.string().min(1),
  description: z.string(),
});

export const shapes = {
  record_decision: {
    title: z.string().min(1).describe("Short name of the decision"),
    rationale: z.string().describe("Why, in one or two sentences"),
    affects: z.array(z.string()).optional().describe("Paths or areas the decision affects"),
    reversible: z.boolean().optional().describe("Whether it is cheap to undo (default true)"),
  },
  ask_user: {
    question: z.string().min(1),
    options: z.array(z.string()).optional().describe("Suggested answers"),
    allow_free_text: z.boolean().optional().describe("Allow an answer outside `options` (default true)"),
  },
  reply: {
    message: z.string().min(1).describe("Your answer, in Markdown; the user reads it in the flow"),
  },
  write_summary: {
    done: z.array(z.string()).describe("What this section accomplished"),
    remaining: z.array(z.string()).describe("What is left to do"),
    follow_ups: z.array(followUp).optional().describe("Work that deserves its own workflow"),
    notes: z.string().optional(),
  },
  record_outcome: {
    kind: z.enum(["pr", "commit", "branch", "claude_artifact", "none"]),
    ref: z.string().describe("PR URL, commit SHA, branch name, or the Claude artifact's claude.ai link; empty for none"),
    summary: z.string(),
    follow_ups: z.array(followUp).optional(),
  },
  message_slot: {
    to: z.string().min(1).describe("The other slot: its number (\"slot 2\"), name or title"),
    message: z.string().min(1).describe("What its agent needs to know, in Markdown"),
  },
  present_artifact: {
    path: z.string().min(1).describe("The file, absolute or relative to the checkout"),
    title: z.string().min(1).describe("What it shows, in a few words"),
    description: z.string().optional().describe("One or two sentences of context"),
  },
  present_review: {
    findings: z
      .array(
        z.object({
          level: z.enum(["low", "mid", "high"]).describe("high: a bug, data loss or security; mid: fix before merge; low: a nit"),
          title: z.string().min(1).describe("What is wrong, in one line"),
          detail: z.string().describe("Why it matters and what a fix would be"),
          path: z.string().optional().describe("File, relative to the checkout"),
          line: z.number().int().positive().optional(),
          suggested: z.enum(["fix", "dismiss", "decide"]).describe("What you recommend; decide = the user's call"),
        }),
      )
      .min(1),
    summary: z.string().optional().describe("What you reviewed, and the overall verdict"),
  },
  request_slot: {
    name: z.string().min(1).describe("Short name for the new slot"),
    purpose: z.string().describe("What the new workflow should do"),
    from_follow_up: z.string().optional().describe("Follow-up id this slot pursues"),
  },
} as const;

type Infer<S extends z.ZodRawShape> = z.infer<z.ZodObject<S>>;
// Two-way assignability between zod schemas and generated protocol types.
type Same<A, B> = [A] extends [B] ? ([B] extends [A] ? true : false) : false;
const schemaMatchesProtocol: {
  record_decision: Same<Infer<typeof shapes.record_decision>, RecordDecisionInput>;
  ask_user: Same<Infer<typeof shapes.ask_user>, AskUserInput>;
  write_summary: Same<Infer<typeof shapes.write_summary>, WriteSummaryInput>;
  reply: Same<Infer<typeof shapes.reply>, ReplyInput>;
  record_outcome: Same<Infer<typeof shapes.record_outcome>, RecordOutcomeInput>;
  request_slot: Same<Infer<typeof shapes.request_slot>, RequestSlotInput>;
  present_artifact: Same<Infer<typeof shapes.present_artifact>, PresentArtifactInput>;
  present_review: Same<Infer<typeof shapes.present_review>, PresentReviewInput>;
  message_slot: Same<Infer<typeof shapes.message_slot>, MessageSlotInput>;
} = {
  record_decision: true,
  ask_user: true,
  write_summary: true,
  reply: true,
  record_outcome: true,
  request_slot: true,
  present_artifact: true,
  present_review: true,
  message_slot: true,
};
void schemaMatchesProtocol;

/** Identifies one tool invocation; `toolUseId` comes from Claude Code's MCP `_meta`. */
export interface CallContext {
  toolUseId?: string;
}

export type ToolOutcome = { ok: true; text: string } | { ok: false; error: string };

/** Implemented by the session; owns blocking, attribution, and round rules. */
export interface ToolHost {
  recordDecision(input: RecordDecisionInput, call: CallContext): Promise<ToolOutcome>;
  askUser(input: AskUserInput, call: CallContext): Promise<ToolOutcome>;
  writeSummary(input: WriteSummaryInput, call: CallContext): Promise<ToolOutcome>;
  reply(input: ReplyInput, call: CallContext): Promise<ToolOutcome>;
  recordOutcome(input: RecordOutcomeInput, call: CallContext): Promise<ToolOutcome>;
  requestSlot(input: RequestSlotInput, call: CallContext): Promise<ToolOutcome>;
  presentArtifact(input: PresentArtifactInput, call: CallContext): Promise<ToolOutcome>;
  presentReview(input: PresentReviewInput, call: CallContext): Promise<ToolOutcome>;
  messageSlot(input: MessageSlotInput, call: CallContext): Promise<ToolOutcome>;
}

/** Run berth tool `name` on `host`; `args` were checked against its schema. */
export function callTool(host: ToolHost, name: ToolName, args: unknown, call: CallContext): Promise<ToolOutcome> {
  switch (name) {
    case "record_decision":
      return host.recordDecision(args as RecordDecisionInput, call);
    case "ask_user":
      return host.askUser(args as AskUserInput, call);
    case "write_summary":
      return host.writeSummary(args as WriteSummaryInput, call);
    case "reply":
      return host.reply(args as ReplyInput, call);
    case "record_outcome":
      return host.recordOutcome(args as RecordOutcomeInput, call);
    case "request_slot":
      return host.requestSlot(args as RequestSlotInput, call);
    case "present_artifact":
      return host.presentArtifact(args as PresentArtifactInput, call);
    case "present_review":
      return host.presentReview(args as PresentReviewInput, call);
    case "message_slot":
      return host.messageSlot(args as MessageSlotInput, call);
  }
}

/** The berth tools on offer: `message_slot` only when Settings allows
 * messages between slots (ADR 0032). */
export function offeredTools(options: ServerOptions): ToolName[] {
  return (Object.keys(shapes) as ToolName[]).filter((name) => name !== "message_slot" || options.slotMessages);
}

/** Fully qualified name the model sees, e.g. `mcp__berth__ask_user`. */
export function qualifiedToolName(name: ToolName): string {
  return `mcp__${MCP_SERVER_NAME}__${name}`;
}

/** Inverse of {@link qualifiedToolName}; undefined for non-berth tools. */
export function berthToolName(qualified: string): ToolName | undefined {
  const prefix = `mcp__${MCP_SERVER_NAME}__`;
  if (!qualified.startsWith(prefix)) return undefined;
  const name = qualified.slice(prefix.length);
  return name in TOOLS ? (name as ToolName) : undefined;
}

export function callContext(extra: unknown): CallContext {
  if (typeof extra !== "object" || extra === null) return {};
  const meta = (extra as { _meta?: unknown })._meta;
  if (typeof meta !== "object" || meta === null) return {};
  const id = (meta as Record<string, unknown>)["claudecode/toolUseId"];
  return typeof id === "string" ? { toolUseId: id } : {};
}

function result(outcome: ToolOutcome) {
  return outcome.ok
    ? { content: [{ type: "text" as const, text: outcome.text }] }
    : { content: [{ type: "text" as const, text: outcome.error }], isError: true };
}

/** Which of the tools are on offer: `message_slot` only when Settings
 * allows messages between slots (ADR 0032). */
export interface ServerOptions {
  slotMessages: boolean;
}

export function createBerthServer(
  host: ToolHost,
  options: ServerOptions = { slotMessages: true },
): McpSdkServerConfigWithInstance {
  return createSdkMcpServer({
    name: MCP_SERVER_NAME,
    version: VERSION,
    alwaysLoad: true,
    tools: [
      tool("record_decision", TOOLS.record_decision.description, shapes.record_decision, async (args, extra) =>
        result(await host.recordDecision(args, callContext(extra))),
      ),
      tool("ask_user", TOOLS.ask_user.description, shapes.ask_user, async (args, extra) =>
        result(await host.askUser(args, callContext(extra))),
      ),
      tool("write_summary", TOOLS.write_summary.description, shapes.write_summary, async (args, extra) =>
        result(await host.writeSummary(args, callContext(extra))),
      ),
      tool("reply", TOOLS.reply.description, shapes.reply, async (args, extra) =>
        result(await host.reply(args, callContext(extra))),
      ),
      tool("record_outcome", TOOLS.record_outcome.description, shapes.record_outcome, async (args, extra) =>
        result(await host.recordOutcome(args, callContext(extra))),
      ),
      tool("request_slot", TOOLS.request_slot.description, shapes.request_slot, async (args, extra) =>
        result(await host.requestSlot(args, callContext(extra))),
      ),
      tool("present_artifact", TOOLS.present_artifact.description, shapes.present_artifact, async (args, extra) =>
        result(await host.presentArtifact(args, callContext(extra))),
      ),
      tool("present_review", TOOLS.present_review.description, shapes.present_review, async (args, extra) =>
        result(await host.presentReview(args, callContext(extra))),
      ),
      ...(options.slotMessages
        ? [
            tool("message_slot", TOOLS.message_slot.description, shapes.message_slot, async (args, extra) =>
              result(await host.messageSlot(args, callContext(extra))),
            ),
          ]
        : []),
    ],
  });
}
