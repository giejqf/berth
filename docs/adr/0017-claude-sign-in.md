# ADR 0017: Signing Claude Code in, and switching accounts

- Status: accepted
- Date: 2026-10-20
- Touches: AGENTS.md §6.3, §9, §11

## Context

Berth has no sign-in of its own: agents on the host run as whatever the
`claude` CLI signs in as. Signing in opened Terminal with `claude auth login`,
but Berth checked again only at launch or when Settings opened, so it went on
saying "not signed in" after a successful login. There was no switch: you
signed out, then in. After a switch the footer's plan and the usage ring's
limits stayed the old account's until a session reported, and running
sessions were left as they were. Starting a workflow while signed out failed
inside the first round with an authentication error.

`claude auth status --json` (probed against 0.3.302's CLI) reports
`loggedIn`, `authMethod` (`claude.ai`, `api_key`, `oauth_token`,
`third_party`, `none`), `apiProvider` (`firstParty`, `bedrock`, `vertex`, …),
`apiKeySource` for keys, and for a subscription `email`, `orgId`, `orgName`,
`subscriptionType`. API keys and cloud providers set in the environment
report `loggedIn: true`. `claude auth login` takes `--claudeai` (default),
`--console` and `--sso`. Signed in already, it replaces the account (Claude
Code's own `/login` is how it switches accounts).

## Decisions

1. **Check on return.** A repo window or the welcome window coming to the
   front re-checks the tools (`RefreshEnvironment`, at most every 5 s), so
   finishing a login in Terminal shows as soon as you come back.
2. **One sign-in dialog** (`ui/sign_in.rs`) for signing in and switching: a
   Claude subscription, the Anthropic Console, or single sign-on, then
   "Continue in Terminal" (`claude auth login --claudeai|--console|--sso`). Signed
   in, it is **Switch Claude account**: it runs the login directly, with no
   sign-out first, so abandoning it keeps the old account. Settings › Claude
   Code has Switch account… beside Sign out, ⌘K has it, and the composer, New
   workflow and Welcome open it where you need it.
3. **Signed out blocks up front.** On a host slot, the composer says "Claude
   Code isn't signed in, so the agent can't run." with Sign in…, and does not
   send. New workflow does the same and won't create the workflow. Before
   starting a session, the backend checks again if the last check said
   signed out, and refuses with where to sign in. That covers workflows
   started elsewhere: follow-ups and the agent's slot requests. `inside` slots
   sign their agent in themselves (§6.1) and are never blocked.
4. **An account change.** Each check records who Claude Code signs in as:
   method, provider, email and organization (`settings.claude_identity`; the
   first check only records it). When that changes, Berth forgets the saved
   account and plan limits (`settings.account`, `settings.quota`; the ring
   loses its plan arc until a session reports again), and restarts every
   session on the host. Idle ones stop at once, and ones that are working,
   waiting on an answer or running something in the background stop when
   their round ends or the background empties. The next prompt resumes the
   conversation by id on the new account. The repo's slash command lists are
   kept: they come from the config directory and plugins, not the account.
5. **Environment setups are ready.** An API key (`ANTHROPIC_API_KEY`,
   `apiKeyHelper`), a token, or a cloud provider shows as ready ("API key",
   "via Bedrock") with Check again only: Berth can't sign them in or out. A
   Console login can be switched and signed out like a subscription.
6. **The footer reads the check.** The plan ("Max plan") and the hover ("Signed
   in as … · org · plan") come from `claude auth status`. A session's
   `accountInfo` fills in only when the check names no plan.

## Consequences

- Restarting a session costs a resume on its next prompt. A round that is
  running is never interrupted for a switch.
- A check that doesn't answer (a timeout) blocks nothing and changes nothing.
- Signing out stays a separate action with its confirmation.
