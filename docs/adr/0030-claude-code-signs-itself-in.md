# ADR 0030: Claude Code signs itself in

- Status: accepted
- Date: 2026-10-31
- Touches: AGENTS.md §11; amends ADR 0017 (decision 2)

## Context

Berth's sign-in dialog (ADR 0017) offered three ways to sign in, a Claude
subscription first and preselected, and then ran `claude auth login
--claudeai|--console|--sso` in Terminal. Anthropic's terms for products that
run Claude Code (code.claude.com/docs/en/legal-and-compliance, "Authentication
and credential use") draw the line here: third-party developers may not "offer
Claude.ai login into their own applications", nor "collect, store, or
intermediate Claude.ai credentials or session tokens" ("sign-in to a Claude
account must complete through Anthropic's own flow"); but nothing prevents "an
end user from signing in to the unmodified Claude Code binary with their own
Claude subscription", and a product running Claude Code may not "remove,
disable, or restrict any authentication method built into it". Berth runs the
unmodified binary (ADR 0029) and never sees a credential, but its dialog was
its own claude.ai login offer.

`claude auth login` with no flag isn't neutral: it signs in to a subscription.
Claude Code's own `/login` asks which account: a Claude subscription, the
Anthropic Console, or a 3rd-party platform (Bedrock, Foundry, Vertex).
`claude "/login"` opens Claude Code with that menu.

## Decisions

1. **No sign-in of Berth's own.** Sign in to Claude Code and Switch Claude
   account (Settings › Claude Code, Welcome, ⌘K, the composer, New workflow)
   open Terminal with `claude "/login"` in the home folder: Claude Code's own
   menu, where the user picks the account. The dialog and its choices are
   gone (`LoginMethod`, `SignInDialog`).
2. **No restriction either.** Whatever Claude Code signs in with, agents on
   this Mac run on it, as before; Berth doesn't refuse a subscription.
3. **Berth's example scripts pass on no subscription token.** The `docker`
   slot type forwards `ANTHROPIC_API_KEY` into its container, no longer
   `CLAUDE_CODE_OAUTH_TOKEN`.

## Consequences

- Signing in takes the user through Claude Code's own screens, which also
  start a session: they leave it with `/exit` once signed in. Berth sees the
  account when one of its windows comes back to the front (ADR 0017, decision 1).
- Anthropic's Agent SDK page still asks SDK-based products to use API key
  authentication "unless previously approved"; Berth relies on the terms for
  products running the unmodified Claude Code above. If that reading changes,
  the place to act is where `backend/env.rs` reads the account
  (`claude_status`), not the sign-in.
