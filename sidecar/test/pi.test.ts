// PiSession with the real Pi (the workspace's devDependency) against a
// scripted OpenAI-compatible server: no tokens spent (ADR 0033).

import { mkdtempSync } from "node:fs";
import http from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { silentLogger } from "../src/log.js";
import { NUDGE_TEXT } from "../src/host.js";
import { PiSession, thinkingLevel } from "../src/pi.js";
import type { SessionStart, SidecarToApp } from "../src/protocol.js";
import { RecordingEmitter } from "./fakes.js";

interface Call {
  name: string;
  args: Record<string, unknown>;
}
/** What the scripted model answers a request with. */
type Answer = { text: string } | { calls: Call[] };
interface Request {
  auth?: string;
  tools: string[];
  messages: Array<{ role: string; content?: unknown; tool_calls?: unknown }>;
  system: string;
}

let server: http.Server;
let base = "";
const requests: Request[] = [];
let script: (req: Request) => Answer = () => ({ text: "ok" });

beforeAll(async () => {
  process.env.BERTH_PI_API_KEY = "sk-test";
  server = http.createServer((req, res) => {
    let body = "";
    req.on("data", (c: Buffer) => (body += c.toString()));
    req.on("end", () => {
      const j = JSON.parse(body) as {
        model: string;
        tools?: Array<{ function: { name: string } }>;
        messages: Request["messages"];
      };
      const first = j.messages[0];
      const r: Request = {
        auth: req.headers.authorization,
        tools: (j.tools ?? []).map((t) => t.function.name),
        messages: j.messages,
        system: typeof first?.content === "string" ? first.content : "",
      };
      requests.push(r);
      const answer = script(r);
      res.writeHead(200, { "content-type": "text/event-stream" });
      const chunk = (delta: object, finish: string | null = null) =>
        res.write(
          `data: ${JSON.stringify({ id: "c", object: "chat.completion.chunk", created: 1, model: j.model, choices: [{ index: 0, delta, finish_reason: finish }] })}\n\n`,
        );
      if ("text" in answer) {
        chunk({ role: "assistant", content: answer.text });
        chunk({}, "stop");
      } else {
        answer.calls.forEach((c, i) =>
          chunk({
            ...(i === 0 ? { role: "assistant" } : {}),
            tool_calls: [{ index: i, id: `call_${requests.length}_${i}`, type: "function", function: { name: c.name, arguments: JSON.stringify(c.args) } }],
          }),
        );
        chunk({}, "tool_calls");
      }
      res.write(
        `data: ${JSON.stringify({ id: "c", object: "chat.completion.chunk", created: 1, model: j.model, choices: [], usage: { prompt_tokens: 900, completion_tokens: 20, total_tokens: 920 } })}\n\n`,
      );
      res.end("data: [DONE]\n\n");
    });
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const addr = server.address();
  base = `http://127.0.0.1:${typeof addr === "object" && addr ? addr.port : 0}/v1`;
});

afterAll(() => {
  server.close();
});

function startMsg(cwd: string, extra: Partial<SessionStart> = {}): SessionStart {
  return {
    workflow_id: "wf-pi",
    cwd,
    system_prompt: "Origin: add a greeting",
    env: {},
    allowed_tools: [],
    connector_skills: [],
    slot_messages: true,
    agent: "pi",
    model: "m1",
    pi: { base_url: base, api: "chat", models: [{ id: "m1", context_window: 64_000 }] },
    ...extra,
  };
}

function setup(extra: Partial<SessionStart> = {}, agentDir = mkdtempSync(join(tmpdir(), "berth-pi-agent-"))) {
  const cwd = mkdtempSync(join(tmpdir(), "berth-pi-co-"));
  const emitter = new RecordingEmitter();
  const session = new PiSession({ emitter, log: silentLogger, agentDir });
  session.start(startMsg(cwd, extra), "start-1");
  return { emitter, session, cwd, agentDir };
}

async function until<T>(get: () => T | undefined, ms = 10_000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = get();
    if (v !== undefined) return v;
    if (Date.now() > end) throw new Error("timed out");
    await new Promise((r) => setTimeout(r, 10));
  }
}

const ended = (e: RecordingEmitter, n = 1) =>
  until(() => (e.ofType("round.ended").length >= n ? e.ofType("round.ended")[n - 1] : undefined));

function toolNames(r: Request): string[] {
  return r.tools.slice().sort();
}

describe("PiSession (ADR 0033)", () => {
  it("runs a round with Berth's tools and ends it on the terminal call", async () => {
    requests.length = 0;
    script = (r) =>
      r.messages.some((m) => m.role === "tool")
        ? r.messages.filter((m) => m.role === "tool").length === 1
          ? { calls: [{ name: "write_summary", args: { done: ["Added greet()"], remaining: [] } }] }
          : { text: "done" }
        : { calls: [{ name: "record_decision", args: { title: "Plain function", rationale: "Simplest" } }] };
    const { emitter, session } = setup();
    session.turn("Add a greeting");
    const end = await ended(emitter);
    expect(end).toMatchObject({ terminal_kind: "write_summary", nudges: 0 });
    const started = emitter.sent.find((s) => s.body.type === "session.started");
    expect(started?.replyTo).toBe("start-1");
    expect(emitter.ofType("node.decision")[0]).toMatchObject({ title: "Plain function", reversible: true });
    expect(emitter.ofType("node.summary")[0]).toMatchObject({ done: ["Added greet()"] });

    // The key went in the request, not through Pi's config.
    expect(requests[0]?.auth).toBe("Bearer sk-test");
    expect(process.env.BERTH_PI_API_KEY).toBeUndefined();
    // Pi's own tools, Berth's, and Berth's prompt after Pi's.
    expect(toolNames(requests[0] as Request)).toEqual(
      ["ask_user", "bash", "edit", "message_slot", "present_artifact", "present_review", "read", "record_decision", "record_outcome", "reply", "request_slot", "write", "write_summary"],
    );
    expect(requests[0]?.system).toContain("You are working inside Berth.");
    expect(requests[0]?.system).toContain("Origin: add a greeting");
    expect(requests[0]?.system).not.toContain("AskUserQuestion");
    expect(requests[0]?.system).not.toContain("Subagents");

    // Raw events in the SDK's envelope, each message with its entry id.
    const raw = emitter.ofType("raw.event");
    expect(raw.map((r) => r.kind)).toEqual(["system", "user", "assistant", "user", "assistant", "user", "assistant", "result"]);
    const call = raw[2]?.json as { message: { content: Array<{ type: string; name?: string }> }; uuid?: string };
    expect(call.message.content[0]).toMatchObject({ type: "tool_use", name: "mcp__berth__record_decision" });
    expect(raw.filter((r) => r.kind !== "system" && r.kind !== "result").every((r) => typeof (r.json as { uuid?: unknown }).uuid === "string")).toBe(true);
    expect(emitter.ofType("session.context").at(-1)).toMatchObject({ max_tokens: 64_000 });
    session.stop();
    await session.done;
  });

  it("nudges a round that ends without a terminal call", async () => {
    requests.length = 0;
    script = (r) =>
      r.messages.at(-1)?.role === "tool"
        ? { text: "done" }
        : JSON.stringify(r.messages).includes(NUDGE_TEXT)
          ? { calls: [{ name: "reply", args: { message: "Hi!" } }] }
          : { text: "Hello" };
    const { emitter, session } = setup();
    session.turn("Say hi");
    expect(await ended(emitter)).toMatchObject({ terminal_kind: "reply", nudges: 1 });
    expect(emitter.ofType("node.reply")[0]).toMatchObject({ message: "Hi!" });
    session.stop();
    await session.done;
  });

  it("waits on the user's answer, then goes on in the same round", async () => {
    script = (r) => {
      const tools = r.messages.filter((m) => m.role === "tool");
      if (tools.length === 0) return { calls: [{ name: "ask_user", args: { question: "Which name?", options: ["greet", "hello"] } }] };
      if (tools.length === 1) return { calls: [{ name: "write_summary", args: { done: ["Named it"], remaining: [] } }] };
      return { text: "done" };
    };
    const { emitter, session } = setup();
    session.turn("Name it");
    const q = await until(() => emitter.ofType("node.question")[0]);
    expect(q).toMatchObject({ question: "Which name?", options: ["greet", "hello"] });
    session.answer(q.question_id, "greet");
    expect(await ended(emitter)).toMatchObject({ terminal_kind: "write_summary" });
    expect(JSON.stringify(requests.at(-2)?.messages)).toContain("The user answered: greet");
    session.stop();
    await session.done;
  });

  it("resumes its conversation, and a rewind branches it", async () => {
    script = (r) =>
      r.messages.some((m) => m.role === "tool") ? { text: "done" } : { calls: [{ name: "reply", args: { message: "noted" } }] };
    const first = setup();
    first.session.turn("Remember: alpha");
    await ended(first.emitter);
    first.session.turn("Remember: beta");
    await ended(first.emitter, 2);
    const id = first.emitter.ofType("session.started")[0]?.claude_session_id as string;
    // The rewind point: the last entry of the first round (ADR 0009).
    const raw = first.emitter.ofType("raw.event");
    const firstResult = raw.findIndex((r) => r.kind === "result");
    const at = (raw.slice(0, firstResult).filter((r) => r.kind === "user" || r.kind === "assistant").at(-1)?.json as { uuid: string }).uuid;
    first.session.stop();
    await first.session.done;

    const resumed = new PiSession({ emitter: new RecordingEmitter(), log: silentLogger, agentDir: first.agentDir });
    resumed.start({ ...startMsg(first.cwd), claude_session_id: id }, "r");
    resumed.turn("What did I say?");
    await ended(resumed["emitter"] as RecordingEmitter);
    const sent = JSON.stringify(requests.at(-2)?.messages);
    expect(sent).toContain("alpha");
    expect(sent).toContain("beta");
    resumed.stop();
    await resumed.done;

    const rewound = new PiSession({ emitter: new RecordingEmitter(), log: silentLogger, agentDir: first.agentDir });
    rewound.start({ ...startMsg(first.cwd), claude_session_id: id, resume_at: at }, "r");
    rewound.turn("What did I say?");
    await ended(rewound["emitter"] as RecordingEmitter);
    const kept = JSON.stringify(requests.at(-2)?.messages);
    expect(kept).toContain("alpha");
    expect(kept).not.toContain("beta");
    rewound.stop();
    await rewound.done;
  });

  it("leaves message_slot out when Settings turns messages off", async () => {
    requests.length = 0;
    script = (r) => (r.messages.at(-1)?.role === "tool" ? { text: "done" } : { calls: [{ name: "reply", args: { message: "ok" } }] });
    const { emitter, session } = setup({ slot_messages: false });
    session.turn("hi");
    await ended(emitter);
    expect(requests[0]?.tools).not.toContain("message_slot");
    expect(requests[0]?.system).not.toContain("message_slot");
    session.stop();
    await session.done;
  });

  it("takes a message sent mid-round into the run (ADR 0018)", async () => {
    script = (r) => {
      const all = JSON.stringify(r.messages);
      if (r.messages.at(-1)?.role === "tool" && all.includes("also say bye")) return { text: "done" };
      if (all.includes("also say bye")) return { calls: [{ name: "write_summary", args: { done: ["Said hi and bye"], remaining: [] } }] };
      return { calls: [{ name: "bash", args: { command: "sleep 0.3" } }] };
    };
    const { emitter, session } = setup();
    session.turn("say hi");
    // While the first command runs, the user adds to it.
    await until(() => (requests.length > 0 ? true : undefined));
    session.turn("also say bye");
    expect(await ended(emitter)).toMatchObject({ terminal_kind: "write_summary" });
    expect(emitter.ofType("round.ended")).toHaveLength(1);
    session.stop();
    await session.done;
  });

  it("ends an interrupted round as interrupted", async () => {
    script = (r) => (r.messages.at(-1)?.role === "tool" ? { text: "stopped" } : { calls: [{ name: "bash", args: { command: "sleep 5" } }] });
    const { emitter, session } = setup();
    session.turn("take your time");
    await until(() => emitter.ofType("raw.event").find((e) => e.kind === "assistant"));
    await session.interrupt();
    expect(await ended(emitter)).toMatchObject({ terminal_kind: "interrupted" });
    session.stop();
    await session.done;
  });

  it("compacts on /compact, a round of its own", async () => {
    script = (r) =>
      JSON.stringify(r.messages).includes("Remember")
        ? r.messages.at(-1)?.role === "tool"
          ? { text: "done" }
          : { calls: [{ name: "reply", args: { message: "ok" } }] }
        : { text: "## Goal\nRemember alpha." };
    const { emitter, session } = setup();
    session.turn("Remember: alpha");
    await ended(emitter);
    session.turn("/compact");
    expect(await ended(emitter, 2)).toMatchObject({ terminal_kind: "command", nudges: 0 });
    expect(emitter.ofType("node.compacted")[0]).toMatchObject({ trigger: "manual" });
    session.stop();
    await session.done;
  });

  it("refuses to start without a provider, and maps efforts", async () => {
    const emitter = new RecordingEmitter();
    const session = new PiSession({ emitter, log: silentLogger, agentDir: mkdtempSync(join(tmpdir(), "berth-pi-")) });
    const msg = startMsg("/tmp");
    delete msg.pi;
    session.start(msg, "s");
    await session.done;
    const err = emitter.sent.find((s) => s.body.type === "session.error") as { body: SidecarToApp; replyTo?: string };
    expect(err.replyTo).toBe("s");
    expect(err.body).toMatchObject({ fatal: true });
    expect([thinkingLevel(null), thinkingLevel("high"), thinkingLevel("max")]).toEqual(["off", "high", "xhigh"]);
  });
});
