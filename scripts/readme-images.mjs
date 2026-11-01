// Writes the README's images in docs/images/: the badges, each value read
// from the file that defines it, and logo.svg, the app icon
// (packaging/AppIcon.svg) with the mark it draws written into it, since an SVG
// shown as an image can't load another file. `just readme-images` runs it; CI
// fails when the committed files are stale. The screenshot,
// docs/images/berth.png, is scripts/readme-screenshot.sh's.
//
// usage: node scripts/readme-images.mjs

import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";

const root = new URL("../", import.meta.url);
const read = (path) => readFileSync(new URL(path, root), "utf8");
const write = (path, text) => writeFileSync(new URL(path, root), text);

function match(path, pattern, what) {
  const found = read(path).match(pattern);
  if (!found) throw new Error(`${path}: can't find ${what}`);
  return found[1];
}

// badge-maker is a dev dependency of the sidecar, the repo's one package.
const require = createRequire(new URL("sidecar/package.json", root));
const { makeBadge } = await import(pathToFileURL(require.resolve("badge-maker")).href);

const version = match("Cargo.toml", /^version = "([^"]+)"/m, "the workspace version");
const license = match("Cargo.toml", /^license = "([^"]+)"/m, "the workspace license");
const macos = match(
  "scripts/bundle.sh",
  /<key>LSMinimumSystemVersion<\/key><string>(\d+)(?:\.0)*<\/string>/,
  "LSMinimumSystemVersion",
);
const sdk = JSON.parse(read("sidecar/package.json")).dependencies["@anthropic-ai/claude-agent-sdk"];
if (!/^\d+\.\d+\.\d+$/.test(sdk ?? "")) {
  throw new Error("sidecar/package.json: the Claude Agent SDK isn't pinned to one version");
}

// Gruvbox's faded accents, which keep white text readable.
const badges = {
  version: { label: "version", message: version, color: "#076678" },
  macos: { label: "macOS", message: `${macos}+`, color: "#427b58" },
  agent: { label: "Claude Agent SDK", message: sdk, color: "#af3a03" },
  license: { label: "license", message: license, color: "#79740e" },
};
mkdirSync(new URL("docs/images/badges/", root), { recursive: true });
for (const [name, format] of Object.entries(badges)) {
  const svg = makeBadge({ ...format, labelColor: "#3c3836", style: "flat", idSuffix: name });
  write(`docs/images/badges/${name}.svg`, `${svg}\n`);
}

// The icon draws the mark twice, as a glow and on top of it: one symbol, used
// twice.
const icon = read("packaging/AppIcon.svg");
const mark = read("crates/berth-app/assets/logo.svg");
const markBody = mark
  .replace(/^[\s\S]*?<svg[^>]*>/, "")
  .replace(/<\/svg>\s*$/, "")
  .replace(/\s*<title>[^<]*<\/title>/, "");
const markBox = match("crates/berth-app/assets/logo.svg", /viewBox="([^"]+)"/, "the mark's viewBox");
const images = /<image href="\.\.\/crates\/berth-app\/assets\/logo\.svg"/g;
if ((icon.match(images) ?? []).length === 0) {
  throw new Error("packaging/AppIcon.svg: can't find where it draws the mark");
}
const logo = icon
  .replace(/\s*<!--[\s\S]*?-->/, "\n  <!-- GENERATED from packaging/AppIcon.svg by scripts/readme-images.mjs; do not edit. -->")
  .replace(/ xmlns:xlink="[^"]*"/, "")
  .replace("<defs>", `<defs>\n    <symbol id="mark" viewBox="${markBox}">${markBody}\n    </symbol>`)
  .replace(images, '<use href="#mark"');
write("docs/images/logo.svg", logo);

console.log(`docs/images: logo.svg, badges/${Object.keys(badges).join(", ")}`);
