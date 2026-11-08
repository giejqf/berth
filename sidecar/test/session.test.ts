import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { HookInput } from "@anthropic-ai/claude-agent-sdk";
import { describe, expect, it } from "vitest";

import { silentLogger } from "../src/log.js";
import type { SessionStart } from "../src/protocol.js";
import {
  MAX_NUDGES,
  NUDGE_TEXT,
  SUBAGENT_TOOL_ERROR,
  Session,
  artifactLink,
  subagentLimitText,
} from "../src/session.js";
import { RecordingEmitter, fakeQueryFn, flush, sdk } from "./fakes.js";

const START: SessionStart = {
  workflow_id: "wf-1",
  cwd: "/tmp/checkout",
  system_prompt: "Origin: fix the bug",
  env: { FOO: "bar" },
  allowed_tools: [],
  slot_messages: true,
  agent: "claude",
  connector_skills: [{ name: "linear", description: "Linear issues" }],
};

function setup() {
  const emitter = new RecordingEmitter();
  const q = fakeQueryFn();
  let now = 1_000;
  const session = new Session({ emitter, log: silentLogger, query: q.fn, now: () => now });
  session.start(START, "start-1");
  return {
    emitter,
    session,
    q: q.last(),
    tick: (ms: number) => {
      now += ms;
    },
  };
}

const MCP = (name: string) => `mcp__berth__${name}`;

describe("session start", () => {
  it("configures the SDK and replies with session.started", async () => {
    const { emitter, q, session } = setup();
    expect(q.options?.permissionMode).toBe("bypassPermissions");
    expect(q.options?.allowDangerouslySkipPermissions).toBe(true);
    expect(q.options?.cwd).toBe("/tmp/checkout");
    expect(q.options?.mcpServers).toHaveProperty("berth");
    expect(q.options?.disallowedTools).toContain("AskUserQuestion");
    const sp = q.options?.systemPrompt;
    expect(sp && typeof sp === "object" && "append" in sp ? sp.append : "").toContain("Origin: fix the bug");
    expect(sp && typeof sp === "object" && "append" in sp ? sp.append : "").toContain("linear: Linear issues");
    expect(q.options?.env?.FOO).toBe("bar");

    q.send(sdk.init("abc"));
    await flush();
    const started = emitter.sent.find((s) => s.body.type === "session.started");
    expect(started?.replyTo).toBe("start-1");
    expect(started?.body).toMatchObject({ claude_session_id: "abc" });
    // Raw events are numbered and verbatim.
    expect(emitter.ofType("raw.event")[0]).toMatchObject({ seq: 1, kind: "system" });

    session.start(START, "again");
    expect(emitter.ofType("session.error").at(-1)).toMatchObject({ fatal: false });
  });

  it("passes resume through", () => {
    const emitter = new RecordingEmitter();
    const q = fakeQueryFn();
    const session = new Session({ emitter, log: silentLogger, query: q.fn });
    session.start({ ...START, claude_session_id: "old" }, "r");
    expect(q.last().options?.resume).toBe("old");
    expect(q.last().options?.resumeSessionAt).toBeUndefined();
  });

  it("resumes a rewound conversation at the kept entry", () => {
    const q = fakeQueryFn();
    const session = new Session({ emitter: new RecordingEmitter(), log: silentLogger, query: q.fn });
    session.start({ ...START, claude_session_id: "old", resume_at: "u-41" }, "r");
    expect(q.last().options).toMatchObject({ resume: "old", resumeSessionAt: "u-41" });
    expect(q.last().options?.forkSession).toBeUndefined();
  });
});

describe("round termination (§5.3)", () => {
  it("ends with write_summary and reports the cost delta", async () => {
    const { emitter, session, q, tick } = setup();
    session.turn("do it");
    await flush();
    expect(q.prompts).toEqual(["do it"]);

    q.send(sdk.toolUse("t1", MCP("write_summary"), { done: ["a"], remaining: [] }));
    await flush();
    const r = await session.writeSummary(
      { done: ["a"], remaining: [], follow_ups: [{ title: "Later", description: "d" }] },
      { toolUseId: "t1" },
    );
    expect(r.ok).toBe(true);
    const summary = emitter.ofType("node.summary")[0];
    expect(summary?.follow_ups[0]?.id).toMatch(/[0-9a-f-]{36}/);

    tick(500);
    q.send(sdk.result(0.25));
    await flush();
    expect(emitter.ofType("round.ended")).toEqual([
      { type: "round.ended", terminal_kind: "write_summary", cost_usd: 0.25, duration_ms: 500, nudges: 0 },
    ]);
    expect(emitter.ofType("session.cost").at(-1)).toMatchObject({ total_usd: 0.25 });

    // Next round's cost is a delta from the running total.
    session.turn("more");
    await session.recordOutcome({ kind: "pr", ref: "https://x/pull/1", summary: "s" }, {});
    q.send(sdk.result(0.4));
    await flush();
    expect(emitter.ofType("round.ended")[1]?.terminal_kind).toBe("record_outcome");
    expect(emitter.ofType("round.ended")[1]?.cost_usd).toBeCloseTo(0.15);
  });

  it("nudges up to MAX_NUDGES times, then ends unterminated with the last text", async () => {
    const { emitter, session, q } = setup();
    session.turn("go");
    for (let i = 0; i < MAX_NUDGES; i++) {
      q.send(sdk.text(`thinking ${i}`));
      q.send(sdk.result(0.1 * (i + 1)));
      await flush();
    }
    expect(q.prompts).toEqual(["go", NUDGE_TEXT, NUDGE_TEXT]);
    expect(emitter.ofType("round.ended")).toHaveLength(0);
    q.send(sdk.text("final words"));
    q.send(sdk.result(0.5));
    await flush();
    expect(emitter.ofType("round.ended")[0]).toMatchObject({
      terminal_kind: "unterminated",
      nudges: MAX_NUDGES,
      last_text: "final words",
    });
  });

  it("a message sent while the agent works carries the round on, unnudged", async () => {
    const { emitter, session, q } = setup();
    session.turn("go");
    q.send(sdk.text("working"));
    await flush();
    session.turn("also this");
    // The turn ended before the message could fold in: it runs next.
    q.send(sdk.result(0.1, "success", 1, 1));
    await flush();
    expect(q.prompts).toEqual(["go", "also this"]);
    expect(emitter.ofType("round.ended")).toHaveLength(0);
    await session.writeSummary({ done: ["both"], remaining: [] }, {});
    q.send(sdk.result(0.2));
    await flush();
    expect(emitter.ofType("round.ended")).toHaveLength(1);
    expect(emitter.ofType("round.ended")[0]).toMatchObject({ terminal_kind: "write_summary", nudges: 0 });
  });

  it("a reply shows the answer and ends the round as reply", async () => {
    const { emitter, session, q } = setup();
    session.turn("why TOML?");
    expect((await session.reply({ message: "Because **comments** survive." }, {})).ok).toBe(true);
    expect(emitter.ofType("node.reply")[0]).toMatchObject({ message: "Because **comments** survive." });
    const second = await session.writeSummary({ done: [], remaining: [] }, {});
    expect(second.ok).toBe(false);
    q.send(sdk.result(0.1));
    await flush();
    expect(q.prompts).toEqual(["why TOML?"]);
    expect(emitter.ofType("round.ended")[0]).toMatchObject({ terminal_kind: "reply", nudges: 0 });
  });

  it("a nudge that produces a terminal call ends the round normally", async () => {
    const { emitter, session, q } = setup();
    session.turn("go");
    q.send(sdk.result(0.1));
    await flush();
    await session.writeSummary({ done: [], remaining: [] }, {});
    q.send(sdk.result(0.2));
    await flush();
    expect(emitter.ofType("round.ended")[0]).toMatchObject({ terminal_kind: "write_summary", nudges: 1 });
  });

  it("rejects a second terminal call; the first wins", async () => {
    const { emitter, session } = setup();
    session.turn("go");
    expect((await session.writeSummary({ done: [], remaining: [] }, {})).ok).toBe(true);
    const second = await session.recordOutcome({ kind: "none", ref: "", summary: "x" }, {});
    expect(second.ok).toBe(false);
    expect(emitter.ofType("node.outcome")).toHaveLength(0);
  });

  it("an error result without a terminal call ends the round with error", async () => {
    const { emitter, session, q } = setup();
    session.turn("go");
    q.send(sdk.result(0.1, "error_during_execution"));
    await flush();
    expect(emitter.ofType("round.ended")[0]?.terminal_kind).toBe("error");
  });
});

describe("message_slot (ADR 0021)", () => {
  it("asks the app to route it and returns what the app says", async () => {
    const { emitter, session } = setup();
    session.turn("tell the backend");
    const pending = session.messageSlot({ to: "backend", message: "The API 500s on /users" }, {});
    await flush();
    const asked = emitter.ofType("slot.message")[0];
    expect(asked).toMatchObject({ to: "backend", message: "The API 500s on /users" });
    session.resolveSlotMessage(asked?.request_id ?? "", true, "Delivered to backend.");
    expect(await pending).toEqual({ ok: true, text: "Delivered to backend." });
  });

  it("reports a refusal as a tool error", async () => {
    const { emitter, session } = setup();
    session.turn("go");
    const pending = session.messageSlot({ to: "nope", message: "hi" }, {});
    await flush();
    session.resolveSlotMessage(emitter.ofType("slot.message")[0]?.request_id ?? "", false, "No slot named nope.");
    expect(await pending).toEqual({ ok: false, error: "No slot named nope." });
  });

  it("does not wait past an interrupt", async () => {
    const { session } = setup();
    session.turn("go");
    const pending = session.messageSlot({ to: "backend", message: "hi" }, {});
    await flush();
    await session.interrupt();
    expect(await pending).toMatchObject({ ok: false });
  });

  it("is refused inside a subagent", async () => {
    const { emitter, session, q } = setup();
    session.turn("go");
    q.send(sdk.toolUse("agent-1", "Agent", { description: "d", prompt: "p" }));
    q.send(sdk.toolUse("m1", MCP("message_slot"), { to: "b", message: "x" }, "agent-1"));
    await flush();
    const r = await session.messageSlot({ to: "b", message: "x" }, { toolUseId: "m1" });
    expect(r).toEqual({ ok: false, error: SUBAGENT_TOOL_ERROR });
    expect(emitter.ofType("slot.message")).toHaveLength(0);
  });
});

describe("claude artifact outcome (ADR 0020)", () => {
  it("takes the title from the Artifact tool's publish", async () => {
    const { emitter, session, q } = setup();
    session.turn("publish a dashboard");
    q.send(sdk.toolUse("a1", "Artifact", { file_path: "dash.html" }));
    q.send(
      sdk.toolResult("a1", "Published", {
        url: "https://claude.ai/code/artifact/5f3e",
        path: "dash.html",
        title: "Release dashboard",
      }),
    );
    // Listing artifacts names none as published.
    q.send(sdk.toolUse("a2", "Artifact", { action: "list" }));
    q.send(sdk.toolResult("a2", "[]", { artifacts: [{ title: "Old", url: "https://claude.ai/artifact/old" }] }));
    await flush();
    const r = await session.recordOutcome(
      { kind: "claude_artifact", ref: "claude.ai/code/artifact/5f3e/", summary: "Published it" },
      {},
    );
    expect(r.ok).toBe(true);
    expect(emitter.ofType("node.outcome")[0]).toMatchObject({
      kind: "claude_artifact",
      ref: "https://claude.ai/code/artifact/5f3e",
      title: "Release dashboard",
    });
  });

  it("refuses a ref that is not an artifact link, and the round can still end", async () => {
    const { emitter, session } = setup();
    session.turn("go");
    const bad = await session.recordOutcome(
      { kind: "claude_artifact", ref: "https://claude.ai/chat/123", summary: "s" },
      {},
    );
    expect(bad).toMatchObject({ ok: false });
    const good = await session.recordOutcome(
      { kind: "claude_artifact", ref: "https://claude.ai/artifact/abc", summary: "s" },
      {},
    );
    expect(good.ok).toBe(true);
    const outcome = emitter.ofType("node.outcome");
    expect(outcome).toHaveLength(1);
    expect(outcome[0]).not.toHaveProperty("title");
  });

  it("reads artifact links in one form", () => {
    expect(artifactLink("https://claude.ai/artifact/abc?x=1#y")).toBe("https://claude.ai/artifact/abc");
    expect(artifactLink(" claude.ai/code/artifact/u-1 ")).toBe("https://claude.ai/code/artifact/u-1");
    expect(artifactLink("http://claude.ai/artifact/abc")).toBeUndefined();
    expect(artifactLink("https://evil.example/artifact/abc")).toBeUndefined();
    expect(artifactLink("https://claude.ai/artifact/abc/edit")).toBeUndefined();
    expect(artifactLink("")).toBeUndefined();
  });
});

describe("ask_user", () => {
  it("blocks until answered, then the same round continues", async () => {
    const { emitter, session, q } = setup();
    session.turn("go");
    const pending = session.askUser({ question: "Which DB?", options: ["sqlite", "pg"] }, {});
    await flush();
    const question = emitter.ofType("node.question")[0];
    expect(question).toMatchObject({ question: "Which DB?", options: ["sqlite", "pg"], allow_free_text: true });

    let settled = false;
    void pending.then(() => (settled = true));
    await flush();
    expect(settled).toBe(false);

    session.answer(question?.question_id ?? "", "sqlite");
    expect(await pending).toEqual({ ok: true, text: "The user answered: sqlite" });

    // The answered question does not count as the round's terminal call.
    expect((await session.writeSummary({ done: ["x"], remaining: [] }, {})).ok).toBe(true);
    q.send(sdk.result(0.1));
    await flush();
    expect(emitter.ofType("round.ended")[0]?.terminal_kind).toBe("write_summary");
  });

  it("interrupt rejects the pending question and ends the round as interrupted", async () => {
    const { emitter, session, q } = setup();
    session.turn("go");
    const pending = session.askUser({ question: "?" }, {});
    await flush();
    await session.interrupt();
    expect(q.interrupts).toBe(1);
    expect(await pending).toEqual({ ok: false, error: "Interrupted by the user." });
    q.send(sdk.result(0.1, "error_during_execution"));
    await flush();
    expect(emitter.ofType("round.ended")[0]?.terminal_kind).toBe("interrupted");
  });

  it("an unknown answer is a non-fatal error", () => {
    const { emitter, session } = setup();
    session.answer("nope", "x");
    expect(emitter.ofType("session.error")[0]).toMatchObject({ fatal: false });
  });
});

describe("present_review", () => {
  const findings = [
    { level: "high" as const, title: "Unchecked unwrap", detail: "Panics.", path: "src/a.rs", line: 4, suggested: "fix" as const },
    { level: "low" as const, title: "Naming", detail: "Shadows std.", suggested: "decide" as const },
  ];

  it("blocks until the decisions arrive, and like an answer they resume the round", async () => {
    const { emitter, session, q } = setup();
    session.turn("review this");
    const pending = session.presentReview({ findings, summary: "Two things." }, {});
    await flush();
    const review = emitter.ofType("node.review")[0];
    expect(review).toMatchObject({ findings, summary: "Two things." });

    // Pending, it is the round's terminal call.
    expect((await session.writeSummary({ done: [], remaining: [] }, {})).ok).toBe(false);

    session.answerReview(review?.review_id ?? "", "1. fix it");
    expect(await pending).toEqual({ ok: true, text: "The user's decisions on your findings:\n1. fix it" });
    expect((await session.writeSummary({ done: ["fixed"], remaining: [] }, {})).ok).toBe(true);
    q.send(sdk.result(0.1));
    await flush();
    expect(emitter.ofType("round.ended")[0]?.terminal_kind).toBe("write_summary");
  });

  it("interrupt rejects the pending review", async () => {
    const { session } = setup();
    session.turn("go");
    const pending = session.presentReview({ findings }, {});
    await flush();
    await session.interrupt();
    expect(await pending).toEqual({ ok: false, error: "Interrupted by the user." });
  });

  it("is not available to subagents, and an unknown review is a non-fatal error", async () => {
    const { emitter, session, q } = setup();
    session.turn("go");
    q.send(sdk.toolUse("agent-9", "Agent", { description: "x", prompt: "p" }));
    q.send(sdk.toolUse("pr1", "mcp__berth__present_review", { findings }, "agent-9"));
    await flush();
    expect(await session.presentReview({ findings }, { toolUseId: "pr1" })).toEqual({
      ok: false,
      error: SUBAGENT_TOOL_ERROR,
    });
    session.answerReview("nope", "x");
    expect(emitter.ofType("session.error")[0]).toMatchObject({ fatal: false });
  });
});

describe("Settings' limits (ADR 0032)", () => {
  function startWith(extra: Partial<SessionStart>) {
    const emitter = new RecordingEmitter();
    const q = fakeQueryFn();
    const session = new Session({ emitter, log: silentLogger, query: q.fn });
    session.start({ ...START, ...extra }, "start-1");
    const options = q.last().options;
    const sp = options?.systemPrompt;
    const prompt = sp && typeof sp === "object" && "append" in sp ? (sp.append ?? "") : "";
    const server = options?.mcpServers?.berth as unknown as { instance: { _registeredTools: Record<string, unknown> } };
    return { emitter, session, q: q.last(), options, prompt, tools: Object.keys(server.instance._registeredTools) };
  }
  const preToolUse = (id: string, toolName = "Agent") =>
    ({ hook_event_name: "PreToolUse", tool_name: toolName, tool_input: {}, tool_use_id: id }) as unknown as HookInput;

  it("offers everything by default", () => {
    const { options, prompt, tools } = startWith({});
    expect(prompt).toContain("message_slot");
    expect(prompt).toContain("Subagents may only call");
    expect(prompt).not.toContain("at a time");
    expect(tools).toContain("message_slot");
    expect(options?.disallowedTools).toEqual(["AskUserQuestion"]);
    expect(options?.hooks).toBeUndefined();
  });

  it("leaves message_slot out when messages are off", () => {
    const { prompt, tools } = startWith({ slot_messages: false });
    expect(prompt).not.toContain("message_slot");
    expect(prompt).not.toContain("Other slots");
    expect(tools).not.toContain("message_slot");
    expect(tools).toContain("ask_user");
  });

  it("leaves Claude Code's Agent tool out without subagents", () => {
    const { options, prompt } = startWith({ max_subagents: 0 });
    expect(options?.disallowedTools).toEqual(expect.arrayContaining(["AskUserQuestion", "Agent", "Task"]));
    expect(prompt).not.toContain("Subagents");
    expect(options?.hooks).toBeUndefined();
  });

  it("holds a spawn back past the limit, and counts background agents until they finish", async () => {
    const { emitter, session, q, options, prompt } = startWith({ max_subagents: 2 });
    expect(prompt).toContain("Run at most 2 subagents at a time");
    const matcher = options?.hooks?.PreToolUse?.[0];
    expect(matcher?.matcher).toBe("Agent|Task");
    const hook = matcher?.hooks[0];
    if (!hook) throw new Error("no PreToolUse hook");
    const gate = (id: string, toolName?: string) =>
      hook(preToolUse(id, toolName), id, { signal: new AbortController().signal });
    const held = (r: unknown) =>
      (r as { hookSpecificOutput?: { permissionDecision?: string } }).hookSpecificOutput?.permissionDecision === "deny";

    session.turn("explore");
    // Three spawned together: two start, the third is held back.
    expect(held(await gate("a1"))).toBe(false);
    expect(held(await gate("a2"))).toBe(false);
    const third = await gate("a3");
    expect(held(third)).toBe(true);
    expect(JSON.stringify(third)).toContain("allows 2 subagents at a time");
    // Another tool isn't counted, should the hook see one.
    expect(held(await gate("b1", "Bash"))).toBe(false);
    for (const id of ["a1", "a2", "a3"]) q.send(sdk.toolUse(id, "Agent", { description: id, prompt: "p" }, null, "m1"));
    q.send(sdk.toolResult("a3", "PreToolUse:Agent hook error: …", undefined, true));
    await flush();
    expect(emitter.ofType("branch.collected")).toEqual([
      expect.objectContaining({ status: "cancelled", result_text: subagentLimitText(2), tool_calls: 0 }),
    ]);

    // a1 runs in the background: it still counts until its notification.
    q.send(sdk.toolResult("a1", "launched", { status: "async_launched", agentId: "t1" }));
    q.send(sdk.toolResult("a2", [{ type: "text", text: "done" }], { status: "completed", content: [] }));
    await flush();
    expect(held(await gate("a4"))).toBe(false);
    expect(held(await gate("a5"))).toBe(true);
    q.send({
      type: "system",
      subtype: "task_notification",
      task_id: "t1",
      tool_use_id: "a1",
      status: "completed",
      summary: "ok",
      output_file: "/tmp/x",
      uuid: "u",
      session_id: "sess-1",
    });
    await flush();
    // a4 never showed up in the stream; a round's end lets it go.
    q.send(sdk.toolUse("r1", MCP("reply"), { message: "hi" }));
    await session.reply({ message: "hi" }, { toolUseId: "r1" });
    q.send(sdk.result(0));
    await flush();
    expect(emitter.ofType("round.ended")).toHaveLength(1);
    session.turn("again");
    expect(held(await gate("a6"))).toBe(false);
    expect(held(await gate("a7"))).toBe(false);
    expect(held(await gate("a8"))).toBe(true);
  });
});

describe("subagent branches (§5.4)", () => {
  it("spawns, attributes events and decisions, restricts tools, and collects", async () => {
    const { emitter, session, q, tick } = setup();
    session.turn("explore");
    q.send(sdk.toolUse("agent-1", "Agent", { description: "Explore", prompt: "look", subagent_type: "Explore" }, null, "m1"));
    await flush();
    const spawned = emitter.ofType("branch.spawned")[0];
    expect(spawned).toMatchObject({
      parent_tool_use_id: "agent-1",
      agent_name: "Explore",
      description: "Explore",
      prompt: "look",
      parallel_group: "m1",
    });
    const branchId = spawned?.branch_id;

    q.send(sdk.toolUse("e1", "Edit", { file_path: "/tmp/checkout/a.rs" }, "agent-1"));
    q.send(sdk.toolUse("d1", MCP("record_decision"), { title: "T", rationale: "R" }, "agent-1"));
    q.send(sdk.toolUse("s1", MCP("write_summary"), { done: [], remaining: [] }, "agent-1"));
    await flush();
    const raw = emitter.ofType("raw.event").filter((r) => r.branch_id === branchId);
    expect(raw).toHaveLength(3);

    await session.recordDecision({ title: "T", rationale: "R" }, { toolUseId: "d1" });
    expect(emitter.ofType("node.decision")[0]).toMatchObject({ branch_id: branchId, reversible: true, affects: [] });

    const denied = await session.writeSummary({ done: [], remaining: [] }, { toolUseId: "s1" });
    expect(denied).toEqual({ ok: false, error: SUBAGENT_TOOL_ERROR });

    tick(41_000);
    q.send(
      sdk.toolResult("agent-1", [{ type: "text", text: "found it" }], {
        status: "completed",
        content: [{ type: "text", text: "found it" }],
        totalDurationMs: 41_000,
        totalToolUseCount: 7,
      }),
    );
    await flush();
    expect(emitter.ofType("branch.collected")[0]).toEqual({
      type: "branch.collected",
      branch_id: branchId,
      result_text: "found it",
      status: "ok",
      duration_ms: 41_000,
      tool_calls: 7,
      touched_files: ["/tmp/checkout/a.rs"],
    });
  });

  it("collects background agents from task_notification", async () => {
    const { emitter, session, q } = setup();
    session.turn("explore");
    q.send(sdk.toolUse("agent-2", "Agent", { description: "Bg", prompt: "p" }));
    q.send(sdk.toolResult("agent-2", "launched", { status: "async_launched", agentId: "a" }));
    await flush();
    expect(emitter.ofType("branch.collected")).toHaveLength(0);
    q.send({
      type: "system",
      subtype: "task_notification",
      task_id: "a",
      tool_use_id: "agent-2",
      status: "failed",
      summary: "boom",
      output_file: "/tmp/x",
      usage: { total_tokens: 1, tool_uses: 3, duration_ms: 900 },
      uuid: "u",
      session_id: "sess-1",
    });
    await flush();
    expect(emitter.ofType("branch.collected")[0]).toMatchObject({
      result_text: "boom",
      status: "error",
      duration_ms: 900,
      tool_calls: 3,
    });
  });

  it("waits for the stream when the handler runs before the tool_use arrives", async () => {
    const { emitter, session, q } = setup();
    session.turn("explore");
    q.send(sdk.toolUse("agent-3", "Agent", { description: "x", prompt: "p" }));
    await flush();
    const pending = session.recordDecision({ title: "Late", rationale: "r" }, { toolUseId: "d9" });
    await flush();
    expect(emitter.ofType("node.decision")).toHaveLength(0);
    q.send(sdk.toolUse("d9", MCP("record_decision"), { title: "Late", rationale: "r" }, "agent-3"));
    await pending;
    expect(emitter.ofType("node.decision")[0]?.branch_id).toBe(emitter.ofType("branch.spawned")[0]?.branch_id);
  });

  it("falls back to input matching without a tool-use id", async () => {
    const { emitter, session, q } = setup();
    session.turn("explore");
    q.send(sdk.toolUse("agent-4", "Agent", { description: "x", prompt: "p" }));
    q.send(sdk.toolUse("d2", MCP("record_decision"), { title: "M", rationale: "r" }, "agent-4"));
    await flush();
    await session.recordDecision({ title: "M", rationale: "r" }, {});
    expect(emitter.ofType("node.decision")[0]?.branch_id).toBeDefined();
  });
});

describe("request_slot", () => {
  it("blocks until resolved and is not terminal", async () => {
    const { emitter, session } = setup();
    session.turn("go");
    const pending = session.requestSlot({ name: "docs", purpose: "write docs", from_follow_up: "f1" }, {});
    await flush();
    const req = emitter.ofType("slot.request")[0];
    expect(req).toMatchObject({ name: "docs", purpose: "write docs", from_follow_up: "f1" });
    session.resolveSlotRequest(req?.request_id ?? "", true, "slot-9");
    expect(await pending).toEqual({ ok: true, text: JSON.stringify({ approved: true, slot_id: "slot-9" }) });
    expect((await session.writeSummary({ done: [], remaining: [] }, {})).ok).toBe(true);
  });
});

describe("attachments and artifacts", () => {
  it("sends attached images as image blocks and keeps other files as text", async () => {
    const dir = mkdtempSync(join(tmpdir(), "berth-att-"));
    const png = join(dir, "shot.png");
    writeFileSync(png, Buffer.from([0x89, 0x50, 0x4e, 0x47]));
    const { session, q } = setup();
    session.turn("What is wrong here?", [
      { path: png, mime: "image/png" },
      { path: join(dir, "missing.png"), mime: "image/png" },
      { path: join(dir, "notes.pdf"), mime: "application/pdf" },
    ]);
    await flush();
    const content = JSON.parse(q.prompts[0] ?? "[]") as Array<Record<string, unknown>>;
    expect(content).toHaveLength(2);
    expect(content[0]).toEqual({ type: "text", text: "What is wrong here?" });
    expect(content[1]).toMatchObject({
      type: "image",
      source: { type: "base64", media_type: "image/png", data: Buffer.from([0x89, 0x50, 0x4e, 0x47]).toString("base64") },
    });
    session.turn("plain");
    await flush();
    expect(q.prompts[1]).toBe("plain");
  });

  it("present_artifact emits node.artifact with the resolved path and size", async () => {
    const dir = mkdtempSync(join(tmpdir(), "berth-art-"));
    writeFileSync(join(dir, "report.html"), "<h1>hi</h1>");
    const emitter = new RecordingEmitter();
    const q = fakeQueryFn();
    const session = new Session({ emitter, log: silentLogger, query: q.fn });
    session.start({ ...START, cwd: dir }, "s");
    session.turn("make a report");
    const r = await session.presentArtifact({ path: "report.html", title: "Report" }, {});
    expect(r).toEqual({ ok: true, text: 'Shown to the user as "Report".' });
    expect(emitter.ofType("node.artifact")[0]).toMatchObject({
      path: join(dir, "report.html"),
      title: "Report",
      size_bytes: 11,
    });
    const missing = await session.presentArtifact({ path: "nope.png", title: "x" }, {});
    expect(missing.ok).toBe(false);
    // Non-terminal: the round still needs its own terminal call.
    expect((await session.writeSummary({ done: [], remaining: [] }, {})).ok).toBe(true);
  });

  it("is not available to subagents", async () => {
    const { session, q } = setup();
    session.turn("go");
    q.send(sdk.toolUse("agent-9", "Agent", { description: "x", prompt: "p" }));
    q.send(sdk.toolUse("pa1", "mcp__berth__present_artifact", { path: "/x", title: "t" }, "agent-9"));
    await flush();
    const r = await session.presentArtifact({ path: "/x", title: "t" }, { toolUseId: "pa1" });
    expect(r).toEqual({ ok: false, error: SUBAGENT_TOOL_ERROR });
  });
});

describe("model and effort", () => {
  it("starts on the slot's model and effort and reports the exact models on offer", async () => {
    const emitter = new RecordingEmitter();
    const q = fakeQueryFn();
    const session = new Session({ emitter, log: silentLogger, query: q.fn, now: () => 0 });
    session.start({ ...START, model: "opus", effort: "xhigh" }, "start-1");
    await flush();
    expect(q.last().options).toMatchObject({ model: "opus", effort: "xhigh" });
    // Aliases become the model they resolve to; Default and duplicates drop out.
    expect(emitter.ofType("session.models")[0]?.models).toEqual([
      {
        value: "claude-opus-5-5",
        display_name: "Opus",
        description: "Most capable",
        efforts: ["low", "medium", "high", "xhigh", "max"],
      },
      { value: "claude-opus-4-8", display_name: "Opus 4.8", description: "Previous Opus", efforts: [] },
      { value: "haiku", display_name: "Haiku", description: "Fastest", efforts: [] },
    ]);
  });

  it("switches mid-session, and back to the defaults on null", async () => {
    const { session, q } = setup();
    await session.configure("haiku", "low");
    await session.configure(null, null);
    expect(q.configured).toEqual([
      { model: "haiku" },
      { effortLevel: "low" },
      { model: undefined },
      { effortLevel: null },
    ]);
  });
});
