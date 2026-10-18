// Bundle the sidecar into dist/sidecar.mjs for Berth.app/Contents/Resources/sidecar/.
//
//   node build.mjs              bundle only (uses BERTH_CLAUDE_PATH or the SDK default at runtime)
//   node build.mjs --with-claude  also copy the SDK's native Claude Code binary to dist/claude

import { copyFileSync, mkdirSync, rmSync, chmodSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";

const root = dirname(fileURLToPath(import.meta.url));
const dist = join(root, "dist");
rmSync(dist, { recursive: true, force: true });
mkdirSync(dist, { recursive: true });

await build({
  entryPoints: [join(root, "src/index.ts")],
  outfile: join(dist, "sidecar.mjs"),
  bundle: true,
  platform: "node",
  target: "node22",
  format: "esm",
  sourcemap: "linked",
  legalComments: "linked",
  // CommonJS dependencies (pino) need `require` inside an ESM bundle.
  banner: { js: "import { createRequire as __berthRequire } from 'node:module'; const require = __berthRequire(import.meta.url);" },
});

if (process.argv.includes("--with-claude")) {
  const sdkRequire = createRequire(createRequire(import.meta.url).resolve("@anthropic-ai/claude-agent-sdk"));
  const pkg = sdkRequire.resolve(`@anthropic-ai/claude-agent-sdk-darwin-${process.arch}/package.json`);
  const target = join(dist, "claude");
  copyFileSync(join(dirname(pkg), "claude"), target);
  chmodSync(target, 0o755);
}

console.log(`built ${join(dist, "sidecar.mjs")}`);
