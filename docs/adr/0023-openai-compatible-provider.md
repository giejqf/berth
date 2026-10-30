# ADR 0023: Claude Code on an OpenAI-compatible API, through Berth's proxy

- Status: accepted
- Date: 2026-10-30
- Touches: AGENTS.md §1, §3, §4, §9, §11

## Context

Berth's agents are Claude Code, on whatever Claude Code signs in with: a
Claude subscription, the Console, an API key or a cloud provider. Many
people want to run them on other models: a provider they already pay
(OpenAI, DeepSeek, Groq), a router to many (OpenRouter), or a model on
their own machine (Ollama, LM Studio, vLLM). Nearly all of these speak
OpenAI's Chat Completions; none speaks the Messages API Claude Code sends.

Claude Code sends its requests wherever `ANTHROPIC_BASE_URL` points, with
`ANTHROPIC_AUTH_TOKEN` as the bearer, and maps its own model names through
`ANTHROPIC_DEFAULT_{OPUS,SONNET,HAIKU,FABLE}_MODEL`. For a model it doesn't
know it assumes a window, which `CLAUDE_CODE_MAX_CONTEXT_TOKENS` corrects.
It compacts on an error reading "prompt is too long: N tokens > M maximum",
and tries again on rate limits, overloads and broken streams. Probed against
the 0.3.302 CLI: a full turn (streamed text, a tool call, its result, the
answer) runs through a translating server.

§1.2 says Claude Code is the only agent, and the sidecar talks to the Agent
SDK and nothing else. Neither changes: the agent, its tools, its sessions
and Berth's protocol stay Claude Code's. Only the model behind it changes.

## Decisions

1. **A model provider, set in Settings.** Agents on this Mac run on Claude
   Code's account (as before, the default) or on an OpenAI-compatible API:
   its base URL, an API key (Keychain, `com.berth.provider.openai`; optional,
   for a local server), the models on offer (picked from its `/models` list
   or typed), the default among them, and the model for Claude Code's quick
   requests (titles, progress summaries, suggested prompts), else the
   agent's own. Settings in SQLite: `provider`, `openai_base_url`,
   `openai_models`, `openai_model`, `openai_fast_model`, `openai_catalog`
   (`berth_core::provider`).
2. **The proxy is Berth's, in Rust** (`crates/berth-proxy`): a hyper server
   on `127.0.0.1` at a free port, started the first time a session needs it,
   open only to a token made at each launch. Each Messages API request goes
   upstream as Chat Completions, its answer back as a message or, streamed,
   as Messages API events. Claude Code holds the token and never the key.
   Not the sidecar (§1.4: it bridges, it owns no logic), and not a separate
   process to install (LiteLLM).
3. **What translates.** System blocks join into a system message; text and
   images go as parts; a tool's result becomes a `tool` message, images it
   returned a user message after it; tool calls go as functions, names that
   OpenAI's 64 `[A-Za-z0-9_-]` can't hold cut and hashed, and back.
   Thinking (its signatures are Anthropic's), server tools (web search) and
   cache markers are left out; so is the model's reasoning on the way back,
   so a conversation can go back to Claude without blocks Anthropic would
   refuse. Text streams as it comes; tool calls are sent whole once the
   answer ends (providers split them every which way). A ping goes out after
   10 s of nothing, so a reasoning model's silence never looks stalled.
4. **Errors read as Anthropic's.** 401, 403, 404, 429 and 5xx map to their
   Messages API types (503 to `overloaded_error`), each message naming the
   host. A context window overflow, in any provider's wording, becomes
   "prompt is too long: N tokens > M maximum", so Claude Code compacts. A
   stream that breaks off becomes an overloaded error, which it retries.
5. **The proxy learns each model's shape.** A 400 or 422 that names the
   request's shape is answered by sending it again: `max_completion_tokens`
   for `max_tokens` (OpenAI's reasoning models), `max_tokens` capped at the
   model's limit (most cap their output below Claude Code's 32k), a
   parameter the model refuses (`temperature`, `stream_options`, …) left out,
   or, when the prompt fits but the window can't also hold the completion
   budget, a smaller budget for that request. What a model refused holds for
   its later requests until Berth quits, or the URL changes.
6. **Sessions.** A host session under the provider gets `ANTHROPIC_BASE_URL`,
   `ANTHROPIC_AUTH_TOKEN`, the Claude model names mapped to its model (Haiku
   to the quick-tasks model) and the model's window when the provider lists
   it. Keys and providers that would win over the proxy are taken out of its
   environment (`ANTHROPIC_API_KEY`, `CLAUDE_CODE_USE_BEDROCK`, …). The
   proxy also maps any `claude-…` name that reaches it, as live as Settings.
   `inside` slots sign their agent in themselves (§6.1) and are not routed:
   the proxy listens on the host's loopback.
7. **Models per slot, per provider.** The picker (⌘⇧M) and Settings' default
   offer the provider's models under it; they take no effort. A slot keeps
   the model it was given under each provider and runs on the other's
   default meanwhile (`Provider::slot_model`): switching back finds it.
8. **Switching** restarts host sessions as an account switch does (ADR 0017):
   each once it is quiet; the next prompt resumes the conversation by id on
   the other provider. Claude Code's plan limits and account are hidden under
   the provider, and its sessions' reports of them, and of Claude's models,
   are ignored. The "Claude Code" row in Settings, the footer and Welcome say
   what agents run on ("via openrouter.ai"); without a URL or a model, agents
   on this Mac are blocked with Settings… as the fix, in place of Sign in….

## Consequences

- Claude Code prices tokens for models it knows; for another provider's it
  guesses ("costBasis: unknown"). Berth shows no cost, so nothing is wrong
  on screen, but `rounds.cost_usd` holds the guess.
- No thinking blocks reach Claude Code, so "Details" shows no reasoning for
  these models.
- Web search, which runs on Anthropic's side, isn't there; web fetch is.
- A Chat Completions server that misreads tool schemas, or a model too weak
  to use tools, makes a poor agent; Berth can't fix that, only report the
  errors as they come.
- Only Chat Completions: OpenAI's Responses API and Gemini's own are not
  spoken (Gemini's OpenAI-compatible endpoint is).
