// The repo's session history (ADR 0010): Claude Code's own list of sessions
// for a directory and its git worktrees, the ones Claude Code can resume from
// any of them. No session is needed to list it.

import { listSessions, type SDKSessionInfo } from "@anthropic-ai/claude-agent-sdk";

import type { Emitter } from "./ndjson.js";
import type { PastSession } from "./protocol.js";

export type ListFn = (options: { dir: string; includeWorktrees: boolean }) => Promise<SDKSessionInfo[]>;

export function pastSession(s: SDKSessionInfo): PastSession {
  const title = s.customTitle?.trim() || s.summary.trim() || s.firstPrompt?.trim() || "Untitled session";
  return {
    session_id: s.sessionId,
    title,
    ...(s.firstPrompt ? { first_prompt: s.firstPrompt } : {}),
    ...(s.cwd ? { cwd: s.cwd } : {}),
    ...(s.gitBranch ? { git_branch: s.gitBranch } : {}),
    last_modified: s.lastModified,
  };
}

/** Reply to `sessions.list` with `sessions.listed`, or a non-fatal error. */
export async function listPast(dir: string, replyTo: string, emitter: Emitter, list: ListFn = listSessions): Promise<void> {
  try {
    const sessions = await list({ dir, includeWorktrees: true });
    emitter.emit({ type: "sessions.listed", sessions: sessions.map(pastSession) }, replyTo);
  } catch (e) {
    emitter.emit(
      { type: "session.error", message: `Could not list sessions: ${e instanceof Error ? e.message : String(e)}`, fatal: false },
      replyTo,
    );
  }
}
