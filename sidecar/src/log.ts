// pino logger writing to ~/Library/Logs/Berth/sidecar.log (never stdout: that is the protocol).

import { homedir } from "node:os";
import { join } from "node:path";
import pino from "pino";

export type Logger = pino.Logger;

export function createLogger(): Logger {
  if (process.env.BERTH_LOG === "silent") return pino({ level: "silent" });
  const dir = process.env.BERTH_LOG_DIR ?? join(homedir(), "Library", "Logs", "Berth");
  const dest = pino.destination({ dest: join(dir, "sidecar.log"), mkdir: true, sync: false });
  return pino({ level: process.env.BERTH_LOG_LEVEL ?? "info", base: { pid: process.pid } }, dest);
}

export const silentLogger: Logger = pino({ level: "silent" });
