import { describe, expect, it } from "vitest";

import { silentLogger } from "../src/log.js";
import { NUDGE_TEXT, Session } from "../src/session.js";
import { RecordingEmitter, fakeQueryFn, flush, sdk } from "./fakes.js";

const START = {
  workflow_id: "wf-1",
  cwd: "/tmp/checkout",
  system_prompt: "",
  env: {},
  allowed_tools: [],
  connector_skills: [],
};

function setup() {
  const emitter = new RecordingEmitter();
  const q = fakeQueryFn();
  const session = new Session({ emitter, log: silentLogger, query: q.fn, now: () => 0 });
  session.start(START, "s");
  return { emitter, session, q: q.last() };
}

describe("slash commands", () => {
  it("reports the commands on offer, with the terminal-bound ones", async () => {
    const { emitter, q } = setup();
    q.send({ ...sdk.init("abc"), terminal_slash_commands: ["exit", "color"] });
    await flush();
    expect(emitter.ofType("session.commands")[0]).toMatchObject({
      terminal: ["exit", "color"],
      commands: [
        { name: "compact", argument_hint: "<instructions>", builtin: true, aliases: [] },
        { name: "usage", aliases: ["cost"] },
        { name: "code-review", argument_hint: "[focus]", builtin: false },
      ],
    });
    q.send({ type: "system", subtype: "commands_changed", commands: [{ name: "pdf", description: "", argumentHint: "" }], uuid: "u", session_id: "abc" });
    await flush();
    expect(emitter.ofType("session.commands").at(-1)).toMatchObject({ commands: [{ name: "pdf" }], terminal: ["exit", "color"] });
  });

  it("ends a /compact round as a command, with its compaction and no nudge", async () => {
    const { emitter, session, q } = setup();
    q.send(sdk.init("abc"));
    session.turn("/compact");
    q.send({ type: "system", subtype: "status", status: "compacting", uuid: "u", session_id: "abc" });
    q.send({
      type: "system",
      subtype: "compact_boundary",
      compact_metadata: { trigger: "manual", pre_tokens: 17_895, post_tokens: 1_961 },
      uuid: "u",
      session_id: "abc",
    });
    q.send({
      type: "user",
      isReplay: true,
      parent_tool_use_id: null,
      session_id: "abc",
      uuid: "u",
      message: { role: "user", content: "<local-command-stdout>Compacted </local-command-stdout>" },
    });
    q.send(sdk.result(0.01, "success", 0));
    await flush();
    expect(emitter.ofType("node.compacted")[0]).toEqual({
      type: "node.compacted",
      trigger: "manual",
      pre_tokens: 17_895,
      post_tokens: 1_961,
    });
    expect(emitter.ofType("round.ended")[0]).toMatchObject({ terminal_kind: "command", last_text: "Compacted" });
    expect(q.prompts).not.toContain(NUDGE_TEXT);
  });

  it("reads output Claude Code sends as local_command_output", async () => {
    const { emitter, session, q } = setup();
    q.send(sdk.init("abc"));
    session.turn("/usage");
    q.send({ type: "system", subtype: "local_command_output", content: "Session: 34% used\n", uuid: "u", session_id: "abc" });
    q.send(sdk.result(0, "success", 0));
    await flush();
    expect(emitter.ofType("round.ended")[0]).toMatchObject({ terminal_kind: "command", last_text: "Session: 34% used" });
  });

  it("keeps what a command printed as the round's text", async () => {
    const { emitter, session, q } = setup();
    q.send(sdk.init("abc"));
    session.turn("/context");
    q.send(sdk.text("## Context Usage\n\n13.7k / 1m"));
    q.send(sdk.result(0.01, "success", 0));
    await flush();
    expect(emitter.ofType("round.ended")[0]).toMatchObject({
      terminal_kind: "command",
      last_text: "## Context Usage\n\n13.7k / 1m",
    });

    // A turn that runs no model but was not a command (a hook blocked it) is
    // nudged as before, and so is a nudge that ran no model.
    session.turn("tidy up");
    q.send(sdk.result(0, "success", 0));
    await flush();
    expect(q.prompts.at(-1)).toBe(NUDGE_TEXT);
    q.send(sdk.result(0, "success", 0));
    q.send(sdk.result(0, "success", 0));
    await flush();
    expect(emitter.ofType("round.ended").at(-1)?.terminal_kind).toBe("unterminated");

    // A model turn without a terminal call is still nudged.
    session.turn("hello");
    q.send(sdk.text("Hi"));
    q.send(sdk.result(0.02));
    await flush();
    expect(q.prompts.at(-1)).toBe(NUDGE_TEXT);
  });
});
