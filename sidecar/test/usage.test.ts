import { describe, expect, it } from "vitest";

import { silentLogger } from "../src/log.js";
import { Session } from "../src/session.js";
import { quotaFromRateLimit, quotaFromUsage } from "../src/usage.js";
import { RecordingEmitter, USAGE_REPORT, fakeQueryFn, flush, sdk } from "./fakes.js";

const START = {
  workflow_id: "wf-1",
  cwd: "/tmp/checkout",
  system_prompt: "",
  env: {},
  allowed_tools: [],
  slot_messages: true,
  agent: "claude" as const,
  connector_skills: [],
};

describe("usage reports", () => {
  it("maps the plan's windows and leaves extra usage out when it is off", () => {
    expect(quotaFromUsage(USAGE_REPORT)).toEqual({
      complete: true,
      available: true,
      plan: "max",
      windows: [
        { key: "five_hour", utilization: 23, resets_at: Date.parse("2026-10-20T15:40:00Z") },
        { key: "seven_day", utilization: 41 },
        { key: "model:Opus", label: "Opus", utilization: 12 },
      ],
    });
  });

  it("says when plan limits don't apply, and keeps quiet when the report has none", () => {
    expect(quotaFromUsage({ subscription_type: null, rate_limits_available: false, rate_limits: null })).toEqual({
      complete: true,
      available: false,
      windows: [],
    });
    expect(quotaFromUsage({ subscription_type: "pro", rate_limits_available: true, rate_limits: null })).toBe(
      undefined,
    );
  });

  it("reads rate-limit events as fractions and seconds", () => {
    expect(
      quotaFromRateLimit({ status: "allowed_warning", rateLimitType: "five_hour", utilization: 0.82, resetsAt: 1_800_000_000 }),
    ).toEqual({
      complete: false,
      available: true,
      windows: [{ key: "five_hour", status: "warning", utilization: 82, resets_at: 1_800_000_000_000 }],
    });
    expect(quotaFromRateLimit({ status: "rejected", rateLimitType: "overage" })?.windows[0]).toEqual({
      key: "extra_usage",
      status: "rejected",
    });
    expect(quotaFromRateLimit({ status: "allowed" })).toBe(undefined);
  });

  it("reports context after every result and compaction, the plan at most once a minute", async () => {
    const emitter = new RecordingEmitter();
    const q = fakeQueryFn();
    let now = 0;
    const session = new Session({ emitter, log: silentLogger, query: q.fn, now: () => now });
    session.start(START, "s");
    const fake = q.last();
    fake.send(sdk.init("abc"));
    await flush();
    expect(emitter.ofType("session.context")).toHaveLength(1);
    expect(emitter.ofType("session.context")[0]?.categories[0]).toEqual({
      name: "Messages",
      tokens: 40_000,
      kind: "used",
    });
    // No window reported, none sent; one reported (ADR 0025) goes along.
    expect(emitter.ofType("session.context")[0]?.max_tokens).toBeUndefined();
    fake.context = { ...fake.context, maxTokens: 48_000 };
    expect(emitter.ofType("session.quota")).toHaveLength(1);

    session.turn("go");
    fake.send(sdk.result(0.1));
    await flush();
    expect(emitter.ofType("session.context")).toHaveLength(2);
    expect(emitter.ofType("session.context")[1]?.max_tokens).toBe(48_000);
    expect(fake.usageCalls).toBe(1);

    fake.send({ type: "system", subtype: "compact_boundary", session_id: "abc", uuid: "u" });
    fake.send({
      type: "rate_limit_event",
      rate_limit_info: { status: "allowed_warning", rateLimitType: "seven_day", utilization: 0.9 },
      uuid: "u",
      session_id: "abc",
    });
    await flush();
    expect(emitter.ofType("session.context")).toHaveLength(3);
    expect(emitter.ofType("session.quota").at(-1)).toMatchObject({ complete: false, windows: [{ key: "seven_day" }] });
    expect(fake.usageCalls).toBe(1);

    now += 60_000;
    fake.send(sdk.result(0.2));
    await flush();
    expect(fake.usageCalls).toBe(2);
    expect(emitter.ofType("session.quota").at(-1)).toMatchObject({ complete: true });
  });
});
