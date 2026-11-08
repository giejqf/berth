// Where Pi comes from (ADR 0033). Berth.app installs it on first use into
// `BERTH_PI_DIR` (Application Support/Berth/pi/<version>); run from the
// workspace (`just dev`, tests), the sidecar's own devDependency is used. The
// bundle leaves Pi out (build.mjs), so this is the one place it is loaded.

import { realpathSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import type * as PiSdk from "@mariozechner/pi-coding-agent";

const PI = "@mariozechner/pi-coding-agent";

/** TypeBox's `Type`, as far as Berth uses it: a JSON Schema taken as is. */
export interface TypeBox {
  Unsafe<T = unknown>(schema: object): PiSdk.ToolDefinition["parameters"] & { static: T };
}

export interface PiModules {
  pi: typeof PiSdk;
  Type: TypeBox;
}

let loaded: Promise<PiModules> | undefined;

/** Pi's SDK, and the TypeBox its tools are described in. */
export function loadPi(): Promise<PiModules> {
  loaded ??= (async () => {
    const dir = process.env.BERTH_PI_DIR;
    const entry = realpathSync(
      dir ? join(dir, "node_modules", PI, "dist", "index.js") : fileURLToPath(import.meta.resolve(PI)),
    );
    const pi = (await import(pathToFileURL(entry).href)) as typeof PiSdk;
    // pi-ai sits beside pi-coding-agent, in npm's layout and pnpm's alike,
    // and exports the TypeBox Pi validates tool arguments with.
    const ai = join(dirname(entry), "..", "..", "pi-ai", "dist", "index.js");
    const { Type } = (await import(pathToFileURL(ai).href)) as { Type: TypeBox };
    return { pi, Type };
  })();
  return loaded;
}

let apiKey: string | undefined;

/** The provider's key, out of the environment before the agent runs a
 * command: Pi's bash tool would hand it on (ADR 0033). Call once, early. */
export function takeApiKey(): string | undefined {
  const key = process.env.BERTH_PI_API_KEY;
  if (key !== undefined) {
    apiKey = key;
    delete process.env.BERTH_PI_API_KEY;
  }
  return apiKey;
}
