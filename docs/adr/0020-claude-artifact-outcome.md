# ADR 0020: A Claude artifact can be a workflow's Outcome

- Status: accepted
- Date: 2026-10-24
- Touches: AGENTS.md §5.1, §5.2, §10, §11

## Context

Not every workflow ends in code to merge. Claude Code has an `Artifact` tool
that publishes an HTML page to claude.ai (a report, a dashboard, a write-up)
and returns its link and title. Agents in Berth have the tool wherever the
`claude` CLI's account offers it (all tools are pre-approved, §1.6). When the
page was the deliverable, `record_outcome` could only say `none`, the link
went into the summary as text, and nothing in the app could open it.

Berth's own `Artifact` node (`present_artifact`, ADR 0002) is a different
thing: a file the agent shows the user inside Berth. It publishes nothing.

## Decisions

1. **A fifth Outcome kind, `claude_artifact`.** `ref` is the page's claude.ai
   link (`https://claude.ai/artifact/…` or `https://claude.ai/code/artifact/…`).
2. **The sidecar checks the link.** For this kind, `record_outcome` refuses a
   `ref` that is not a claude.ai artifact link (http, another host, another
   path), with a tool error naming the forms, so the agent can call it again;
   the refused call does not take the round's terminal slot. A link is stored
   in one form: https, no query, no trailing slash. A link the session itself
   published is accepted as it was returned, whatever its shape.
3. **The title comes from the publish, not the agent.** The sidecar notes each
   `Artifact` tool result that carries a page (`url` and `path`; listings,
   reads and asset calls don't) and puts its title on the Outcome
   (`Outcome.title`, optional). An artifact published by an earlier sidecar
   process has no title; the card shows the link instead.
4. **The agent is told when to use it.** The system prompt and the tool's
   description say: when the result is a page published with the Artifact
   tool rather than code to merge, end with kind `claude_artifact` and the
   link; `present_artifact` only shows a file.
5. **Rendering.** The Outcome card reads "Outcome · Claude artifact", with a
   window glyph, the page's title (or its link, mono), and **Copy link** and
   **Open artifact** (the browser). The rail says "Artifact published", the
   flow header and the history "Done · artifact".

## Consequences

- Protocol: `OutcomeKind::ClaudeArtifact` and `Outcome.title?`. Both are
  additive: stored Outcome nodes read as before, and a title is written only
  when there is one.
- Like any Outcome it closes the workflow, takes the section's snapshot and
  opens the review pane; a page often comes with no changes to review, which
  the pane already handles.
- The app does not fetch or render the page: it is on claude.ai, behind the
  user's account. Berth only links to it.
