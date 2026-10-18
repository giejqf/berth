// NDJSON framing over stdio (AGENTS.md §10). stdout carries protocol lines only.

import { randomUUID } from "node:crypto";
import { createInterface } from "node:readline";
import type { Readable, Writable } from "node:stream";

import type { AppMessage, SidecarMessage, SidecarToApp } from "./protocol.js";

/** Sends sidecar -> app messages. */
export interface Emitter {
  emit(body: SidecarToApp, replyTo?: string): void;
}

export class NdjsonWriter implements Emitter {
  constructor(private readonly out: Writable) {}

  emit(body: SidecarToApp, replyTo?: string): void {
    const msg: SidecarMessage = { id: randomUUID(), ...(replyTo ? { reply_to: replyTo } : {}), ...body };
    this.out.write(`${JSON.stringify(msg)}\n`);
  }
}

/** Parse one line; returns an error string for malformed input. */
export function parseAppMessage(line: string): AppMessage | string {
  let value: unknown;
  try {
    value = JSON.parse(line);
  } catch (e) {
    return `invalid JSON: ${(e as Error).message}`;
  }
  if (typeof value !== "object" || value === null) return "message is not an object";
  const v = value as Record<string, unknown>;
  if (typeof v.id !== "string") return "message has no string id";
  if (typeof v.type !== "string") return "message has no string type";
  return value as AppMessage;
}

/** Invoke `onLine` for each non-empty line until `input` ends. */
export async function readLines(input: Readable, onLine: (line: string) => void): Promise<void> {
  const rl = createInterface({ input, crlfDelay: Infinity });
  for await (const line of rl) {
    if (line.trim().length > 0) onLine(line);
  }
}
