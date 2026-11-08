// What every agent session does for Berth, whichever agent runs it (Claude
// Code, or Pi under an OpenAI-compatible provider, ADR 0033): Berth's tools
// (§5.2), the round rules (§5.3: one terminal call, nudges), and the
// questions, reviews, slot requests and messages that wait on the app.

import { randomUUID } from "node:crypto";
import { statSync } from "node:fs";
import { isAbsolute, resolve } from "node:path";

import type { Logger } from "./log.js";
import type { Emitter } from "./ndjson.js";
import type {
  AskUserInput,
  FollowUp,
  FollowUpInput,
  MessageSlotInput,
  PresentArtifactInput,
  PresentReviewInput,
  RecordDecisionInput,
  RecordOutcomeInput,
  ReplyInput,
  RequestSlotInput,
  RequestSlotResult,
  TerminalKind,
  WriteSummaryInput,
} from "./protocol.js";
import type { CallContext, ToolHost, ToolOutcome } from "./tools.js";

export const NUDGE_TEXT = "End this round with ask_user, write_summary, reply, or record_outcome.";
export const MAX_NUDGES = 2;
export const SUBAGENT_TOOL_ERROR = "Not available to subagents; report this back to the main agent.";
export const ARTIFACT_REF_ERROR =
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

/** `/name` at the start of a turn: what the agent would run as a command. */
export const SLASH_COMMAND = /^\/[A-Za-z0-9][A-Za-z0-9:._-]*(?:\s|$)/;

export type TerminalTool = "ask_user" | "present_review" | "write_summary" | "record_outcome" | "reply";

export interface Round {
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

export interface Pending<T> {
  resolve(value: T): void;
  reject(err: Error): void;
}

/** Which lane a tool call belongs to: the main agent's, or a subagent's branch (§5.4). */
export interface Lane {
  branchId?: string;
  inSubagent: boolean;
}

export abstract class BerthHost implements ToolHost {
  protected readonly questions = new Map<string, Pending<string>>();
  protected readonly reviews = new Map<string, Pending<string>>();
  protected readonly slotRequests = new Map<string, Pending<RequestSlotResult>>();
  /** `message_slot` calls waiting for the app to say where the message went. */
  protected readonly slotMessages = new Map<string, Pending<ToolOutcome>>();
  /** claude.ai artifacts this session published with Claude Code's Artifact
   * tool: link → title, for an Outcome that names one (ADR 0020). */
  protected readonly published = new Map<string, string | undefined>();
  protected round?: Round;
  protected cwd = "";
  protected totalUsd = 0;

  constructor(
    protected readonly emitter: Emitter,
    protected readonly log: Logger,
    protected readonly now: () => number,
  ) {}

  /** Whose call it is: a tool call from a subagent gets less (§5.4). */
  protected abstract lane(call: CallContext, input: unknown, tool: string): Promise<Lane>;

  /** Send the agent `text` as its next turn, a nudge (§5.3). */
  protected abstract nudge(text: string): void;

  /** The round just ended: what the session keeps per round goes. */
  protected onRoundEnd(): void {}

  /** What waits on the app gives up: questions and reviews fail with `why`,
   * slot requests are declined, messages report they weren't confirmed. */
  protected abandonPending(why: string, messagesWhy: string): void {
    for (const pending of [this.questions, this.reviews]) {
      for (const p of pending.values()) p.reject(new Error(why));
      pending.clear();
    }
    for (const p of this.slotRequests.values()) p.resolve({ approved: false });
    this.slotRequests.clear();
    this.settleSlotMessages(messagesWhy);
  }

  /** The agent's turn finished (Claude Code's `result`, Pi's `agent_end`):
   * end the round, or nudge it toward its terminal call (§5.3). `queued`:
   * messages the user sent meanwhile, which carry the round on (ADR 0018);
   * `numTurns` 0 with a slash command: Claude Code ran it by itself. */
  protected settleRound(isError: boolean, numTurns: number, queued: number): void {
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
    // No model turn for a slash command: the agent ran it by itself
    // (/compact, /context). Nothing to nudge (ADR 0015).
    if (numTurns === 0 && round.nudges === 0 && round.command) return this.endRound("command");
    if (round.nudges < MAX_NUDGES) {
      round.nudges += 1;
      this.nudge(NUDGE_TEXT);
      return;
    }
    this.endRound("unterminated");
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

  protected settleSlotMessages(why: string): void {
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

  protected error(message: string, fatal: boolean, replyTo?: string): void {
    this.log.error({ fatal }, message);
    this.emitter.emit({ type: "session.error", message, fatal }, replyTo);
  }

  /** Accept a terminal call for the current round, or explain why not (§5.3). */
  protected claimTerminal(tool: TerminalTool): string | undefined {
    this.round ??= this.newRound();
    const current = this.round.terminal;
    if (current) {
      return `This round already ended with ${current}; only the first terminal call counts. End your turn now.`;
    }
    this.round.terminal = tool;
    return undefined;
  }

  protected followUps(input: FollowUpInput[] | undefined): FollowUp[] {
    return (input ?? []).map((f) => ({ id: f.id ?? randomUUID(), title: f.title, description: f.description }));
  }

  async recordDecision(input: RecordDecisionInput, call: CallContext): Promise<ToolOutcome> {
    const lane = await this.lane(call, input, "record_decision");
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
    if ((await this.lane(call, input, "ask_user")).inSubagent) return { ok: false, error: SUBAGENT_TOOL_ERROR };
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
    if ((await this.lane(call, input, "present_review")).inSubagent) {
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
    if ((await this.lane(call, input, "write_summary")).inSubagent) {
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
    if ((await this.lane(call, input, "reply")).inSubagent) {
      return { ok: false, error: SUBAGENT_TOOL_ERROR };
    }
    const rejected = this.claimTerminal("reply");
    if (rejected) return { ok: false, error: rejected };
    this.emitter.emit({ type: "node.reply", message: input.message });
    return { ok: true, text: "Reply shown to the user. End your turn now." };
  }

  async recordOutcome(input: RecordOutcomeInput, call: CallContext): Promise<ToolOutcome> {
    if ((await this.lane(call, input, "record_outcome")).inSubagent) {
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
    if ((await this.lane(call, input, "present_artifact")).inSubagent) {
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
    if ((await this.lane(call, input, "message_slot")).inSubagent) {
      return { ok: false, error: SUBAGENT_TOOL_ERROR };
    }
    const requestId = randomUUID();
    const routed = new Promise<ToolOutcome>((resolve, reject) => this.slotMessages.set(requestId, { resolve, reject }));
    this.emitter.emit({ type: "slot.message", request_id: requestId, to: input.to, message: input.message });
    return routed;
  }

  async requestSlot(input: RequestSlotInput, call: CallContext): Promise<ToolOutcome> {
    if ((await this.lane(call, input, "request_slot")).inSubagent) {
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

  protected newRound(): Round {
    return { startedAt: this.now(), startTotalUsd: this.totalUsd, nudges: 0, terminal: null, interrupted: false };
  }

  protected endRound(kind: TerminalKind): void {
    const round = this.round;
    if (!round) return;
    this.round = undefined;
    this.onRoundEnd();
    this.emitter.emit({
      type: "round.ended",
      terminal_kind: kind,
      cost_usd: Math.max(0, this.totalUsd - round.startTotalUsd),
      duration_ms: Math.max(0, this.now() - round.startedAt),
      nudges: round.nudges,
      ...(round.lastText ? { last_text: round.lastText } : {}),
    });
  }
}
