import { PassThrough } from "node:stream";
import { describe, expect, it } from "vitest";

import { run } from "../src/index.js";
import { silentLogger } from "../src/log.js";
import { parseAppMessage } from "../src/ndjson.js";
import type { SidecarMessage } from "../src/protocol.js";
import { callContext, berthToolName, qualifiedToolName } from "../src/tools.js";
import { fakeQueryFn, flush, sdk } from "./fakes.js";

function lines(out: PassThrough): SidecarMessage[] {
  let text = "";
  for (let chunk = out.read() as Buffer | null; chunk !== null; chunk = out.read() as Buffer | null) {
    text += chunk.toString("utf8");
  }
  return text
    .split("\n")
    .filter((l) => l.length > 0)
    .map((l) => JSON.parse(l) as SidecarMessage);
}

describe("stdio NDJSON loop", () => {
  it("dispatches app messages and frames replies", async () => {
    const input = new PassThrough();
    const output = new PassThrough();
    const q = fakeQueryFn();
    const done = run({ input, output, log: silentLogger, query: q.fn });

    input.write(
      `${JSON.stringify({
        id: "1",
        type: "session.start",
        workflow_id: "w",
        cwd: "/tmp",
        system_prompt: "",
        env: {},
        allowed_tools: [],
        connector_skills: [],
      })}\n`,
    );
    input.write(`${JSON.stringify({ id: "2", type: "session.turn", text: "hello" })}\n`);
    input.write("not json\n");
    await flush();
    q.last().send(sdk.init("s-1"));
    await flush();

    const msgs = lines(output);
    expect(q.last().prompts).toEqual(["hello"]);
    const started = msgs.find((m) => m.type === "session.started");
    expect(started).toMatchObject({ reply_to: "1", claude_session_id: "s-1" });
    expect(typeof started?.id).toBe("string");
    expect(msgs.some((m) => m.type === "session.error")).toBe(true);

    input.end();
    await done;
    expect(q.last().closed).toBe(true);
  });
});

describe("helpers", () => {
  it("parses envelopes", () => {
    expect(parseAppMessage('{"id":"1","type":"session.stop"}')).toEqual({ id: "1", type: "session.stop" });
    expect(typeof parseAppMessage('{"type":"x"}')).toBe("string");
    expect(typeof parseAppMessage("[]")).toBe("string");
  });

  it("maps tool names and reads the tool-use id from MCP _meta", () => {
    expect(qualifiedToolName("ask_user")).toBe("mcp__berth__ask_user");
    expect(berthToolName("mcp__berth__write_summary")).toBe("write_summary");
    expect(berthToolName("mcp__other__write_summary")).toBeUndefined();
    expect(callContext({ _meta: { "claudecode/toolUseId": "toolu_1" } })).toEqual({ toolUseId: "toolu_1" });
    expect(callContext(undefined)).toEqual({});
  });
});
