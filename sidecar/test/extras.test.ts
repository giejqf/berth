import { describe, expect, it } from "vitest";

import { renamePast } from "../src/history.js";
import { silentLogger } from "../src/log.js";
import type { SessionStart } from "../src/protocol.js";
import { RESUME_REJECTED, Session } from "../src/session.js";
import { RecordingEmitter, fakeQueryFn, flush, sdk } from "./fakes.js";

const START: SessionStart = {
  workflow_id: "wf-1",
  cwd: "/tmp/checkout",
  system_prompt: "",
  env: {},
  allowed_tools: [],
  connector_skills: [],
  title: "Fix the login redirect",
};

function setup(msg: Parameters<Session["start"]>[0] = START) {
  const emitter = new RecordingEmitter();
  const q = fakeQueryFn();
  const session = new Session({ emitter, log: silentLogger, query: q.fn, now: () => 0 });
  session.start(msg, "s");
  return { emitter, session, q };
}

describe("SDK extras (ADR 0016)", () => {
  it("names new sessions, and asks for summaries, subagent text and suggestions", () => {
    const { q } = setup();
    expect(q.last().options).toMatchObject({
      title: "Fix the login redirect",
      agentProgressSummaries: true,
      forwardSubagentText: true,
      promptSuggestions: true,
    });
    // A resumed session keeps its own title; a rewind declares its one turn.
    const resumed = setup({ ...START, claude_session_id: "s1", resume_at: "a1", drops_turn: "u2" });
    expect(resumed.q.last().options).toMatchObject({ resume: "s1", resumeSessionAt: "a1", resumeDropsTurn: "u2" });
    expect(resumed.q.last().options?.title).toBeUndefined();
  });

  it("sends each turn with its uuid", async () => {
    const { session, q } = setup();
    session.turn("go", [], "11111111-1111-4111-8111-111111111111");
    await flush();
    expect(q.last().messages[0]?.uuid).toBe("11111111-1111-4111-8111-111111111111");
  });

  it("reports summaries, background tasks, refusals, suggestions and the account", async () => {
    const { emitter, session, q } = setup();
    const fake = q.last();
    fake.send(sdk.init("abc"));
    session.turn("go");
    fake.send(sdk.toolUse("agent-1", "Agent", { description: "Survey", prompt: "p" }));
    fake.send({ type: "system", subtype: "task_progress", task_id: "t1", tool_use_id: "agent-1", description: "Survey", usage: { total_tokens: 1, tool_uses: 1, duration_ms: 1 }, summary: "Reading the auth module", uuid: "u", session_id: "abc" });
    fake.send({
      type: "system",
      subtype: "background_tasks_changed",
      tasks: [
        { task_id: "t1", task_type: "local_agent", description: "Survey", subagent_type: "general-purpose" },
        { task_id: "t2", task_type: "local_bash", description: "watcher", ambient: true },
      ],
      uuid: "u",
      session_id: "abc",
    });
    fake.send({ type: "system", subtype: "model_refusal_fallback", trigger: "refusal", direction: "retry", original_model: "claude-opus-5-5", fallback_model: "claude-sonnet-5-5", request_id: null, api_refusal_category: "cyber", content: "", uuid: "u", session_id: "abc" });
    fake.send({ type: "prompt_suggestion", suggestion: " Open a PR ", uuid: "u", session_id: "abc" });
    await flush();
    const branch = emitter.ofType("branch.spawned")[0]?.branch_id;
    expect(emitter.ofType("branch.progress")[0]).toMatchObject({ branch_id: branch, summary: "Reading the auth module" });
    expect(emitter.ofType("session.tasks")[0]?.tasks).toEqual([
      { task_id: "t1", kind: "local_agent", description: "Survey", agent: "general-purpose" },
    ]);
    expect(emitter.ofType("node.refusal")[0]).toMatchObject({
      model: "claude-opus-5-5",
      fallback_model: "claude-sonnet-5-5",
      category: "cyber",
      subagent: false,
    });
    expect(emitter.ofType("session.suggestion")[0]?.text).toBe("Open a PR");
    expect(emitter.ofType("session.account")[0]).toMatchObject({ plan: "Claude Max", provider: "firstParty" });

    await session.background();
    expect(fake.backgrounded).toEqual([undefined]);
  });

  it("resumes the whole conversation when Claude Code refuses the guarded one", async () => {
    const { emitter, session, q } = setup({ ...START, claude_session_id: "s1", resume_at: "a1", drops_turn: "u2" });
    session.turn("(rewound) try again", [], "33333333-3333-4333-8333-333333333333");
    await flush();
    q.last().send({ ...sdk.result(0, "error_during_execution"), errors: [`${RESUME_REJECTED} resuming at a1 would discard …`] });
    await flush();
    expect(q.all).toHaveLength(2);
    const plain = q.last();
    expect(plain.options).toMatchObject({ resume: "s1" });
    expect(plain.options?.resumeSessionAt).toBeUndefined();
    expect(plain.options?.resumeDropsTurn).toBeUndefined();
    // The turn goes again, on the new query; the round did not end.
    expect(plain.messages.map((m) => m.uuid)).toEqual(["33333333-3333-4333-8333-333333333333"]);
    expect(q.all[0]?.closed).toBe(true);
    expect(emitter.ofType("round.ended")).toHaveLength(0);
    expect(emitter.ofType("session.error").at(-1)).toMatchObject({ fatal: false });
  });

  it("renames a session in Claude Code's history", async () => {
    const emitter = new RecordingEmitter();
    const calls: unknown[] = [];
    await renamePast("s1", "Fix login", "/repo", "r1", emitter, (id, title, o) => {
      calls.push([id, title, o.dir]);
      return Promise.resolve();
    });
    expect(calls).toEqual([["s1", "Fix login", "/repo"]]);
    expect(emitter.sent[0]).toMatchObject({ replyTo: "r1", body: { type: "sessions.renamed", ok: true } });
  });
});
