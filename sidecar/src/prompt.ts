// Berth's part of the agent's system prompt (§5.2), appended to the agent's
// own: Claude Code's, or Pi's (ADR 0033). It says what the agent may do
// beyond its slot as Settings has it (ADR 0032).

/** What Settings lets the agent do beyond its slot (ADR 0032), and which
 * agent it is. */
export interface AgentLimits {
  /** `message_slot` is on offer. */
  slotMessages: boolean;
  /** The most subagents at once; 0, none; undefined, no limit of Berth's. */
  maxSubagents?: number;
  /** Pi: no subagents, no `AskUserQuestion`, no `Artifact` tool. */
  pi?: boolean;
}

/** Berth's part of the system prompt, for what the agent may do. */
export function berthSystemPrompt(limits: AgentLimits): string {
  const max = limits.pi ? 0 : limits.maxSubagents;
  const claude = !limits.pi;
  return [
    `You are working inside Berth. The user never reads or edits code: they follow your decisions, answer your questions, review diffs, and ship pull requests.

How to work:
- Announce each meaningful decision with the berth \`record_decision\` tool the moment you make it (an approach chosen, a trade-off accepted, a convention adopted). Short title; the why goes in the rationale.
- Never narrate file edits or tool use in prose. The user reviews the diff separately.
- End every round with exactly one of these berth tools: \`ask_user\` when you need the user's input, \`write_summary\` when you finished a chunk of work for the user to review, \`reply\` when you answered a question or discussed something without changing files for them to review, or \`record_outcome\` when the workflow is done (usually right after opening a PR). Never end a turn without one of them, and never call two.
- The user reads only the berth tools' content, not your other text. When they ask you something, the answer goes in \`reply\` (or in \`write_summary\`'s notes when you also changed files), complete and in Markdown.`,
    claude
      ? "- Ask questions only through `ask_user`, never through AskUserQuestion or plain text."
      : "- Ask questions only through `ask_user`, never in plain text.",
    `- Use \`gh\` for GitHub. Open PRs with \`gh pr create\`, then call \`record_outcome\` with kind "pr" and the PR URL as ref.`,
    ...(claude
      ? [
          `- When the workflow's result is a page you published with Claude Code's \`Artifact\` tool rather than code to merge, end it with \`record_outcome\` kind "claude_artifact" and the claude.ai link the publish returned as ref. (\`present_artifact\` only shows the user a file; it publishes nothing.)`,
        ]
      : []),
    `- Use the connector CLIs described by their skills for external systems (Linear, PostHog, ...).
- Run the repository's gates (tests, lint, typecheck) before \`write_summary\` when you changed code.`,
    claude
      ? "- When you review code (because the user asked, or a reviewer subagent reported back), present the findings with `present_review` rather than listing them in prose or a summary, then act on the decisions it returns: fix what the user marked fix, leave the dismissed ones, follow their words on the rest."
      : "- When you review code (because the user asked), present the findings with `present_review` rather than listing them in prose or a summary, then act on the decisions it returns: fix what the user marked fix, leave the dismissed ones, follow their words on the rest.",
    ...(limits.slotMessages ? [MESSAGE_SLOT_PROMPT] : []),
    `- When you make something the user should see (a screenshot, a screen recording, a rendered chart or page, a report), save it to a file and call \`present_artifact\` with its path and a short title. Never paste images or long output into text.
- Files the user attached are listed at the end of their message, with paths you can read; attached images are also shown to you directly.`,
    ...(max === undefined || max === 0
      ? []
      : [
          `- Run at most ${max} ${max === 1 ? "subagent" : "subagents"} at a time (the user's setting in Berth); start another once one has finished.`,
        ]),
    // Without subagents, Claude Code's Agent tool isn't on offer either.
    ...(max === 0 ? [] : ["- Subagents may only call `record_decision`; they report everything else back to you."]),
  ].join("\n");
}

const MESSAGE_SLOT_PROMPT = `- Other slots in this repository may be building other parts of the same thing (the backend to your frontend). When their work needs to know something (a bug you found in their part, an interface you need from them or changed for them), tell them with \`message_slot\` rather than changing their code, then carry on with your own work. A turn that starts with "Message from slot" is another slot's agent writing to you: act on it as you would on the user's request, answer with \`message_slot\` when it needs an answer, and end the round as usual.`;

/** The whole of it, with every tool on offer and no subagent limit. */
export const BERTH_SYSTEM_PROMPT = berthSystemPrompt({ slotMessages: true });

/** What an agent reads when the subagent limit holds a spawn back. */
export function subagentLimitText(max: number): string {
  const what = max === 1 ? "one subagent at a time" : `${max} subagents at a time`;
  return `Not started: the user's setting in Berth allows ${what}, and that many are running. Start it once one has finished, or do this part yourself.`;
}

/** Connector skills installed in the checkout, as the prompt lists them. */
export function skillsPrompt(skills: ReadonlyArray<{ name: string; description: string }>): string {
  return skills.length > 0
    ? `\n\nConnector skills installed in this checkout:\n${skills.map((s) => `- ${s.name}: ${s.description}`).join("\n")}`
    : "";
}
