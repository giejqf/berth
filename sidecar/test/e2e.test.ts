// Real Claude Code round trip. Runs only with BERTH_E2E=1 (spends real tokens).

import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

import { silentLogger } from "../src/log.js";
import { Session } from "../src/session.js";
import { RecordingEmitter } from "./fakes.js";

const enabled = process.env.BERTH_E2E === "1";

describe.skipIf(!enabled)("e2e: real claude", () => {
  it("records a decision, edits a file, and ends with write_summary", { timeout: 300_000 }, async () => {
    const cwd = mkdtempSync(join(tmpdir(), "berth-e2e-"));
    execFileSync("git", ["init", "-q", "-b", "main"], { cwd });

    const emitter = new RecordingEmitter();
    const session = new Session({ emitter, log: silentLogger });
    session.start(
      {
        workflow_id: "e2e",
        cwd,
        system_prompt: "",
        env: {},
        allowed_tools: [],
        slot_messages: true,
        agent: "claude",
        connector_skills: [],
        ...(process.env.BERTH_E2E_MODEL ? { model: process.env.BERTH_E2E_MODEL } : {}),
      },
      "start",
    );
    session.turn(
      "Create hello.txt containing exactly `hi`. Record one decision about the file name first. " +
        "Do not run tests. Then end the round with write_summary.",
    );

    const deadline = Date.now() + 280_000;
    while (emitter.ofType("round.ended").length === 0 && Date.now() < deadline) {
      await new Promise((r) => setTimeout(r, 250));
    }
    session.stop();
    await session.done;

    expect(emitter.ofType("session.started")).toHaveLength(1);
    expect(emitter.ofType("node.decision").length).toBeGreaterThanOrEqual(1);
    expect(emitter.ofType("node.summary")).toHaveLength(1);
    expect(emitter.ofType("round.ended")[0]?.terminal_kind).toBe("write_summary");
    expect(readFileSync(join(cwd, "hello.txt"), "utf8").trim()).toBe("hi");
  });
});
