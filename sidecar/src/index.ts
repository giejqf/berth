// Sidecar entrypoint: NDJSON over stdio between the app and one agent
// session: Claude Code's (the Agent SDK), or Pi's (ADR 0033).

import { listPast, renamePast } from "./history.js";
import { createLogger, type Logger } from "./log.js";
import { NdjsonWriter, parseAppMessage, readLines, type Emitter } from "./ndjson.js";
import { PiSession, type PiSessionDeps } from "./pi.js";
import { takeApiKey } from "./pi-load.js";
import type { AppMessage, Effort, SessionResume, SessionStart, TurnAttachment } from "./protocol.js";
import { Session, type QueryFn } from "./session.js";

/** What the app's messages drive: either agent's session. */
export interface AgentRunner {
  readonly started: boolean;
  readonly done: Promise<void>;
  start(msg: SessionStart | SessionResume, replyTo: string): void;
  turn(text: string, attachments?: TurnAttachment[], uuid?: string): void;
  interrupt(): Promise<void>;
  stop(): void;
  answer(questionId: string, text: string): void;
  answerReview(reviewId: string, text: string): void;
  resolveSlotMessage(requestId: string, ok: boolean, text: string): void;
  resolveSlotRequest(requestId: string, approved: boolean, slotId?: string): void;
  configure(model: string | null, effort: Effort | null): Promise<void>;
  background(toolUseId?: string): Promise<void>;
  stopTask(taskId: string): Promise<void>;
  readTask(taskId: string): Promise<void>;
}

export function dispatch(session: AgentRunner, msg: AppMessage, emitter: Emitter): void {
  switch (msg.type) {
    case "session.start":
    case "session.resume":
      session.start(msg, msg.id);
      break;
    case "session.turn":
      session.turn(msg.text, msg.attachments ?? [], msg.uuid);
      break;
    case "session.interrupt":
      void session.interrupt();
      break;
    case "session.stop":
      session.stop();
      break;
    case "question.answer":
      session.answer(msg.question_id, msg.text);
      break;
    case "review.answer":
      session.answerReview(msg.review_id, msg.text);
      break;
    case "slot_message.sent":
      session.resolveSlotMessage(msg.request_id, msg.ok, msg.text);
      break;
    case "slot_request.resolved":
      session.resolveSlotRequest(msg.request_id, msg.approved, msg.slot_id);
      break;
    case "session.configure":
      void session.configure(msg.model, msg.effort);
      break;
    case "sessions.list":
      void listPast(msg.dir, msg.id, emitter);
      break;
    case "sessions.rename":
      void renamePast(msg.session_id, msg.title, msg.dir, msg.id, emitter);
      break;
    case "session.background":
      void session.background(msg.tool_use_id);
      break;
    case "task.stop":
      void session.stopTask(msg.task_id);
      break;
    case "task.read":
      void session.readTask(msg.task_id);
      break;
    default: {
      const unknown = msg as { type: string; id: string };
      emitter.emit({ type: "session.error", message: `unknown message type ${unknown.type}`, fatal: false }, unknown.id);
    }
  }
}

export async function run(opts: {
  input: NodeJS.ReadableStream;
  output: NodeJS.WritableStream;
  log: Logger;
  query?: QueryFn;
  pi?: Pick<PiSessionDeps, "load" | "agentDir">;
}): Promise<void> {
  // Out of the environment before any agent runs a command (ADR 0033).
  takeApiKey();
  const emitter = new NdjsonWriter(opts.output as import("node:stream").Writable);
  let session: AgentRunner = new Session({ emitter, log: opts.log, query: opts.query });
  // `session.stop` ends the process as stdin closing does: the app waits for
  // the exit, and stdin stays open until it gives up.
  let stopped!: () => void;
  const stop = new Promise<void>((resolve) => (stopped = resolve));
  const lines = readLines(opts.input as import("node:stream").Readable, (line) => {
    const msg = parseAppMessage(line);
    if (typeof msg === "string") {
      opts.log.warn({ line: line.slice(0, 200) }, msg);
      emitter.emit({ type: "session.error", message: msg, fatal: false });
      return;
    }
    opts.log.debug({ type: msg.type, id: msg.id }, "app message");
    // A Pi session is the process's from its start on.
    if ((msg.type === "session.start" || msg.type === "session.resume") && msg.agent === "pi" && !session.started) {
      session = new PiSession({ emitter, log: opts.log, ...opts.pi });
    }
    dispatch(session, msg, emitter);
    if (msg.type === "session.stop") stopped();
  });
  await Promise.race([lines, stop]);
  // stdin closed, or the app asked us to stop.
  session.stop();
  await session.done;
}

const isMain = import.meta.url === `file://${process.argv[1]}` || process.argv[1]?.endsWith("sidecar.mjs");
if (isMain) {
  const log = createLogger();
  process.on("uncaughtException", (err) => {
    log.fatal({ err }, "uncaught exception");
    process.stdout.write(
      `${JSON.stringify({ id: "fatal", type: "session.error", message: String(err), fatal: true })}\n`,
      () => process.exit(1),
    );
  });
  run({ input: process.stdin, output: process.stdout, log })
    .then(() => process.exit(0))
    .catch((err: unknown) => {
      log.fatal({ err }, "sidecar failed");
      process.exit(1);
    });
}
