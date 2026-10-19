import type { SDKSessionInfo } from "@anthropic-ai/claude-agent-sdk";
import { describe, expect, it } from "vitest";

import { listPast, pastSession } from "../src/history.js";
import { RecordingEmitter } from "./fakes.js";

const info = (over: Partial<SDKSessionInfo>): SDKSessionInfo => ({
  sessionId: "s1",
  summary: "Fix the login bug",
  lastModified: 1_700_000_000_000,
  ...over,
});

describe("session history", () => {
  it("lists the repo's sessions, worktrees included, titled as Claude Code titles them", async () => {
    const emitter = new RecordingEmitter();
    let asked: unknown;
    await listPast("/repo", "req-1", emitter, (options) => {
      asked = options;
      return Promise.resolve([
        info({ cwd: "/repo/.worktrees/a", gitBranch: "fix", firstPrompt: "the login fails" }),
        info({ sessionId: "s2", summary: "", customTitle: "  Renamed  " }),
      ]);
    });
    expect(asked).toEqual({ dir: "/repo", includeWorktrees: true });
    const listed = emitter.ofType("sessions.listed")[0];
    expect(listed?.sessions).toEqual([
      {
        session_id: "s1",
        title: "Fix the login bug",
        first_prompt: "the login fails",
        cwd: "/repo/.worktrees/a",
        git_branch: "fix",
        last_modified: 1_700_000_000_000,
      },
      { session_id: "s2", title: "Renamed", last_modified: 1_700_000_000_000 },
    ]);
  });

  it("falls back to the first prompt, and reports a failure without dying", async () => {
    expect(pastSession(info({ summary: " ", firstPrompt: "hello" })).title).toBe("hello");
    const emitter = new RecordingEmitter();
    await listPast("/repo", "req-2", emitter, () => Promise.reject(new Error("no projects dir")));
    expect(emitter.ofType("session.error")[0]).toMatchObject({ fatal: false });
  });
});
