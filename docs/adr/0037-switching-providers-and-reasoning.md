# ADR 0037: Switching providers, reasoning effort, and models that differ

- Status: accepted
- Date: 2026-11-14
- Touches: AGENTS.md §4, §11, §13; amends ADR 0023 (decisions 3, 5, 6, 7, 8) and
  ADR 0025 (decision 1)

## Context

Tried against Azure OpenAI, agents on an OpenAI-compatible API went wrong in
three ways.

- **Switching** between it and Claude Code's account sometimes needed a
  restart or a rewind. Switching to Claude unset the proxy's upstream at once,
  so a round still running on the API got 401s, which Claude Code reads as a
  failed sign-in and retries for minutes ("Sign-in failed; trying again (8 of
  10)"). Settings' other changes (a model taken out, a new default, a window)
  reached a running session only when it next started.
- **Some models didn't answer right.** Each model takes its own shape, and the
  proxy learned only a few of them, four tries at most, keeping what it
  learned even when the next try failed too. Codex takes only the Responses
  API; most of Azure's other models (DeepSeek, Llama, Grok) only Chat
  Completions. Azure wants its key as `api-key` on its classic endpoints, its
  API under `/openai/v1`, and caps a tool's description at 1024 characters
  (Claude Code's Bash tool is several thousand). DeepSeek-R1 writes its
  reasoning into the answer between `<think>` tags. Claude Code also puts
  `system` messages mid-conversation, which some servers refuse, and long or
  repeated tool call ids from one server break another.
- **Effort did nothing.** The picker offered no effort for the provider's
  models, and the proxy dropped the effort Claude Code sends
  (`output_config.effort`; probed against the 2.1.302 CLI: it sends "high"
  when none is set).

Probed: Claude Code keeps a thinking block, signature and all, and sends it
back while the model stays the same; once the model it talks to is another
one (as named in the answer that carried the block), it drops the block. The
whitespace-only text some models start with is dropped too.

## Decisions

1. **Switching never cuts a session off.** The proxy keeps the API's upstream
   while agents run on Claude Code's account: a session that started on the
   API finishes its round there, then restarts on Claude (as ADR 0023 meant).
   A change to the provider's models or windows restarts each session through
   the proxy whose model or window it changes, once it is quiet (ADR 0017); a
   model no longer on offer runs on the default meanwhile. Picking a model
   with another window mid-session restarts it the same way, so Claude Code
   compacts by the right window.
2. **Effort is the model's reasoning effort.** The picker offers Default,
   Low, Medium, High and Extra high for the provider's models. Each session
   has its own proxy token (`<token>.<workflow id>`), which carries its
   effort; the picker's change reaches the next request. Requests Claude Code
   asks to think in get it as `reasoning.effort` (Responses) or
   `reasoning_effort` (Chat Completions); Max is `xhigh`; Default sends none,
   so the provider's own applies. A model that takes less gets the most it
   takes ("Supported values are…"); one that doesn't reason, none.
3. **The model's reasoning is kept, as thinking Berth signs.** From the
   Responses API the proxy asks for reasoning summaries and the encrypted
   reasoning (`include: reasoning.encrypted_content`; `store` stays false);
   from Chat Completions it reads `reasoning_content` or `reasoning`; `<think>`
   text at the start of an answer is reasoning too, not the answer. Each comes
   to Claude Code as a thinking block whose signature is Berth's
   (`berth-reasoning:…`), so Details shows it. Within the tool loop under
   way, the proxy sends it back: reasoning items before the calls they led to,
   `reasoning_content` on the message. Claude Code drops these blocks when the
   conversation goes on with another model (Claude), so Anthropic never sees
   them; an answer labeled with one of Claude's names carries none. This
   supersedes ADR 0023's "reasoning stays out".
4. **The proxy learns more of each model's shape**, up to eight tries a
   request, and keeps what it learned only once a request in it went through:
   the other API (Codex's "only supported in v1/responses", Azure's "operation
   does not work with the specified model"; a server without `/responses`
   gets Chat Completions, where ADR 0025 said to choose it); the efforts it
   takes; no reasoning summaries or encrypted reasoning; reasoning it can't
   take back (an item that can't be verified, a server without
   `reasoning_content`); no images (a note in their place); a cap on tool
   descriptions (cut there, and given whole in the system prompt).
5. **Azure OpenAI.** A resource's URL as the portal gives it (the resource, a
   deployment's target URI, with or without `api-version`) becomes its v1 API,
   `…/openai/v1`; the key goes as `api-key` too. "DeploymentNotFound" says a
   model is its deployment's name. Its content filter's refusal names what it
   caught ("jailbreak"); an answer it stops ends with a note, not as a refusal
   (Claude Code would call that a breach of Anthropic's usage policy). A rate
   limit's `retry-after` reaches Claude Code. Pi gets the same URL.
6. **What else the proxy evens out.** A `system` message mid-conversation joins
   the user turn it follows as a `<system-reminder>`; whitespace before an
   answer's first words is dropped; a tool call id a server repeats or numbers
   (`0`, `1`) becomes a new one, and an id over 40 characters goes upstream
   hashed to one that fits (Chat Completions' limit), alike for the call and
   its result. `HEAD /api/hello`, Claude Code's look at whether its API is up,
   answers 200.

## Consequences

- Reasoning models do better within a tool loop (they go on from their
  reasoning), and their reasoning shows in Details; it costs the encrypted
  reasoning's size in each request of the loop.
- A model's first request may take a few refusals to find its shape, once per
  launch; a server that neither takes nor names a parameter Berth adds
  (`include`, `reasoning`) still fails the way it did.
- The ADR 0023 consequence "Details shows no reasoning for these models" no
  longer holds where the provider gives summaries.
- `devtools`: `provider_context <model> <tokens>`.
