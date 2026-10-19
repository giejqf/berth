import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

import { silentLogger } from "../src/log.js";
import type { SessionStart } from "../src/protocol.js";
import { MAX_NUDGES, NUDGE_TEXT, SUBAGENT_TOOL_ERROR, Session } from "../src/session.js";
import { RecordingEmitter, fakeQueryFn, flush, sdk } from "./fakes.js";

const START: SessionStart = {
  workflow_id: "wf-1",
  cwd: "/tmp/checkout",
  system_prompt: "Origin: fix the bug",
  env: { FOO: "bar" },
  allowed_tools: [],
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
