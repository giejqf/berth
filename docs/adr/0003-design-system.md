# ADR 0003: The design system (Gruvbox redesign)

- Status: accepted
- Date: 2026-10-18
- Touches: AGENTS.md §3.1, §4, §5.2, §5.4, §11

## Context

The UI was rebuilt to match the design canvas "Berth — UI/UX design" (18
artboards: the visual system, component states, every main-window state in dark
and light, first run, empty repository, both dialogs, subagent lanes, keyboard
notes). The brief was a pixel-perfect match. The artboards are HTML, so matching
them meant matching what Chrome renders, not only the numbers written in them.

## Decisions

### 1. Match the render, not the stated sizes

The artboards use CSS's default `content-box` sizing, so padding and borders add
to stated widths. Berth uses the rendered sizes: the rail is 257px (240 + 2×8
padding + 1 border), the collapsed rail 57px, the review pane 521px, the title bar
45px, dialogs 602 / 642px, text fields 34px (32 + borders), the composer's text
area 64px. Every screen was compared to a Chrome render of its artboard,
pixel-sampled where it mattered (see `devtools`'s `BERTH_DEV_SEED`).

Two browser behaviors are reproduced on purpose:

- A line of 13/18 prose that holds a 12px JetBrains Mono span is 19px tall in
  Chrome (the two fonts' baselines meet; the mono box hangs lower). Inline code
  chips (`kit::code`) and plain mono spans (`kit::code_plain`) are laid out the
  same way: 19px line, chip background on the mono content box (15.84px) 1.6px
  below the line top.
- The design sets `-webkit-font-smoothing: antialiased`. GPUI thickens glyphs
  through CoreGraphics font smoothing unless the app's `AppleFontSmoothing`
  default is `0`, so Berth writes that once into its own defaults domain
  (`com.berth.app`, or the executable name in development). Nothing else is
  touched.

### 2. Fonts and icons are bundled

IBM Plex Sans (400/500/600/700) and JetBrains Mono (400/500/600/700, italic for
comments in diffs) ship in the binary under SIL OFL 1.1. Fonts are data, not code
dependencies, and OFL allows bundling with MIT software. Icons are the design's
own Lucide glyphs (ISC) at their exact stroke widths (1.5 / 1.75 / 2 / 2.5),
embedded by `build.rs`; a three-size spinner glyph replaces CSS's bordered ring.

### 3. Berth draws its own diff and dialogs

gpui-component's `diff::Diff` could not take the design's gutter (36/36/16),
line backgrounds, hunk headers, or comment cards hung at the code column. The
diff is now `ui/diff_view.rs`: a virtualized `list` over `berth-git`'s typed
model, colored by gpui-component's tree-sitter highlighter through a highlight
theme built from the design's five syntax tokens. Dialogs are drawn by the app
(`ui/modal.rs`) for the same reason.

### 4. Subagents are blocks, not lanes

The flow is one column. A group of subagents spawned together is one block with
a row per subagent (3px colored rail, counts, collapsed by default); expanding a
row shows its decisions and collected result. This replaces the side lanes and
chips of ADR 0001 (§5.4 updated); `berth_core::flow::layout` computes it.

### 5. Live activity is distilled in the core

The live card and the rail's second line need more than one line of activity:
`berth_core::activity::RoundActivity` keeps the round's tool steps (phrased
"Reading `main.rs`" / "Read `main.rs`", with start and end times), the agent's
latest sentence, and subagent and tool-call counts. Views still never see raw
events.

## Where the design and the app part ways

- The keyboard notes give ⌘⇧R to "collapse the rail", while the empty-repository
  screen shows ⌘⇧R on "From a pull request". The screen wins (⌘⇧R and ⌘⇧I open New
  workflow on the PR and issue tabs); rail collapse is ⌘⇧L.
- ⌘⇧A is "jump to the slot that needs you"; attaching files moved to ⌘⇧O.
- The working composer's footnote "Queues until the agent asks or summarizes"
  describes a queue the design itself disables; it reads "Unlocks when …".
- Send stays disabled over an empty composer (the design draws it enabled).
- "Post N to PR" counts every comment not yet on the PR, matching what the
  button posts.
- The title bar shows the repository path with `~` for the home directory;
  dialogs keep the rail's buttons visible under the scrim.
- The repository line in the title bar starts right after the window buttons
  (16px after their edge at x = 75) instead of being centered, so the buttons
  and the line read as one bar. Long paths truncate with an ellipsis.
- Where the artboards wrap a button's label for lack of room (dialog footers, the
  review footer), Berth wraps the hint text instead.
