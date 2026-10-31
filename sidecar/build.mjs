// Bundle the sidecar into dist/sidecar.mjs for Berth.app/Contents/Resources/sidecar/.
//
// The Claude Agent SDK and the Claude Code binary it runs are Anthropic's and
// not open source, so the bundle leaves them out (ADR 0029). It loads the SDK
// from `claude-agent-sdk/` beside itself, where the app installs it from npm;
// dist/claude-agent-sdk.json names the exact packages, with the lockfile's
// checksums. Run from the workspace, it falls back to sidecar/node_modules.

import { readFileSync, writeFileSync, mkdirSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";

const SDK = "@anthropic-ai/claude-agent-sdk";

const root = dirname(fileURLToPath(import.meta.url));
const dist = join(root, "dist");
rmSync(dist, { recursive: true, force: true });
mkdirSync(dist, { recursive: true });

const version = JSON.parse(readFileSync(join(root, "package.json"), "utf8")).dependencies[SDK];
if (!/^\d+\.\d+\.\d+$/.test(version)) throw new Error(`${SDK} must be pinned to an exact version, not ${version}`);
const exports = Object.keys(await import(SDK)).filter((name) => name !== "default");

// Every import of the SDK becomes this module: the installed copy's exports.
const shim = `
import { existsSync } from "node:fs";
const installed = new URL("./claude-agent-sdk/sdk.mjs", import.meta.url);
let sdk;
if (existsSync(installed)) sdk = await import(installed.href);
else {
  try {
    sdk = await import(${JSON.stringify(SDK)});
  } catch (e) {
    throw new Error(\`The Claude Agent SDK isn't installed at \${installed.pathname}: Berth installs it there from npm.\`, { cause: e });
  }
}
export const { ${exports.join(", ")} } = sdk;
`;

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
  plugins: [
    {
      name: "claude-agent-sdk",
      setup(b) {
        b.onResolve({ filter: /^@anthropic-ai\/claude-agent-sdk$/ }, (args) =>
          args.namespace === "berth" ? { path: SDK, external: true } : { path: "claude-agent-sdk", namespace: "berth" },
        );
        b.onLoad({ filter: /.*/, namespace: "berth" }, () => ({ contents: shim, loader: "js", resolveDir: root }));
      },
    },
  ],
});

// What the app downloads: the SDK, and the Claude Code for each Mac CPU.
const lock = readFileSync(join(root, "pnpm-lock.yaml"), "utf8");
function pkg(name) {
  const key = `${name}@${version}`.replace(/[.*+?^${}()|[\]\\/]/g, "\\$&");
  const m = lock.match(new RegExp(`\\n  '?${key}'?:\\n    resolution: \\{integrity: sha512-([A-Za-z0-9+/=]+)\\}`));
  if (!m) throw new Error(`pnpm-lock.yaml has no sha512 for ${name}@${version}`);
  return {
    name,
    url: `https://registry.npmjs.org/${name}/-/${name.split("/")[1]}-${version}.tgz`,
    sha512: Buffer.from(m[1], "base64").toString("hex"),
  };
}
const manifest = {
  version,
  sdk: pkg(SDK),
  claude: { aarch64: pkg(`${SDK}-darwin-arm64`), x86_64: pkg(`${SDK}-darwin-x64`) },
};
writeFileSync(join(dist, "claude-agent-sdk.json"), `${JSON.stringify(manifest, null, 2)}\n`);

console.log(`built ${join(dist, "sidecar.mjs")} (${SDK} ${version} left out)`);
