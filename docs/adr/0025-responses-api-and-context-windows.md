# ADR 0025: The Responses API, and each model's context window

- Status: accepted
- Date: 2026-10-30
- Touches: AGENTS.md §4, §9, §10, §11, §13; amends ADR 0023

## Context

ADR 0023 spoke Chat Completions to the provider. OpenAI's current API is the
Responses API (`/v1/responses`): some models take nothing else (Codex), and
more servers speak it each release (OpenRouter, Ollama, LM Studio, vLLM).
Others still speak only Chat Completions (DeepSeek).

Claude Code assumes a window for a model it doesn't know, and compacts by
that; `CLAUDE_CODE_MAX_CONTEXT_TOKENS` corrects it, and Berth set it only
where the provider's `/models` said (OpenRouter, vLLM, not OpenAI). A model
whose window Berth didn't know overflowed it, or compacted too late. And the
usage ring took the window to be the sum of `getContextUsage`'s parts, which
for a small window (Claude Code's tools and prompt alone are ~40k tokens, its
compaction reserve 33k) is larger than the window itself: a nearly full 48k
window read as 56%.

## Decisions

1. **API: Responses (the default) or Chat Completions**, per provider
   (`settings.openai_api`, "chat" for Chat Completions). The proxy's
   translation is one, its wire format two (`berth_proxy::Api`): the system
   prompt goes as `instructions`, turns as `input` items (`function_call`,
   `function_call_output`, `input_image`), tools flat and `strict: false`
   (Claude Code's schemas aren't strict mode's), `store: false` (Claude Code
   sends the whole conversation each time), the output budget as
   `max_output_tokens`. Streams map the same way: text deltas stream, function
   calls gather by output index and go out whole, `response.completed` and
   `.incomplete` end the answer (`max_output_tokens` → `max_tokens`). Reasoning
   items stay out, as thinking does (ADR 0023). A server with no
   `/responses` (404) says to choose Chat Completions.
2. **A context window per model**, set in Settings beside it ("128k", "1m";
   `settings.openai_contexts`), over what the provider lists, which shows as
   the field's placeholder. Sessions get it as
   `CLAUDE_CODE_MAX_CONTEXT_TOKENS`, from their next start.
3. **The proxy uses the window.** A request's output budget is cut to what
   the window leaves after the prompt (estimated, with a margin), so a 32k
   budget doesn't overflow a 32k window; an overflow the provider reports
   without numbers ("Your input exceeds the context window", as the Responses
   API words it, or only `context_length_exceeded`) reads as "prompt is too
   long" with the window as its maximum, and Claude Code compacts.
4. **The window Claude Code reports goes to the ring.** `session.context`
   carries `max_tokens` (`getContextUsage().maxTokens`, optional); the ring's
   share is the used tokens of it, the parts' sum only when it is missing.

## Consequences

- §10 gains `session.context.max_tokens`; older sessions' saved context
  (without it) reads as before.
- With a small window, Claude Code's own prompt and tools take most of it:
  agents on 32k–64k models compact often. The ring now says so.
- Switching API takes effect at the next request, with no restart.
