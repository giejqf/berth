# ADR 0036: Update reminders from GitHub's releases

- Status: accepted
- Date: 2026-11-12
- Touches: AGENTS.md §4, §9, §11, §13

## Context

Berth ships as signed, notarized disk images on GitHub's release page (§12.1),
and nothing told a user a newer one was out: they found out by visiting the
page. Installing in place (download, replace `/Applications/Berth.app`,
relaunch) is the auto-update §13 defers. A reminder is the part worth having
now: it says a newer Berth is out and what it changed, and gets the image
without a trip to the browser.

## Decisions

1. **Upstream is GitHub's releases**, read through its API
   (`GET api.github.com/repos/giejqf/berth/releases`, the repository from
   `Cargo.toml`), unauthenticated, with a `Berth/<version>` user agent. Drafts
   and pre-releases are left out; a release is its tag's `major.minor.patch`
   (`berth_core::update`). Nothing else is sent: no identifier, no usage. It is
   no telemetry (§4); Settings can turn it off.
2. **When it looks.** A built app (one that installs the Claude Agent SDK from
   its Resources, ADR 0029) looks 10 seconds after launch when 12 hours have
   passed since its last look, and again every 12 hours while it runs, unless
   Settings › Updates is Off (`settings.update_check = off`). Check for
   Updates (the Berth menu, ⌘K, Settings' Check now) looks at once. A
   development build looks only when asked, or with `BERTH_UPDATE_URL`.
3. **The reminder is a window of its own**, like macOS's software updates, so
   it shows over whatever Berth has open, Welcome included, and never sits in
   a repo window's one dialog slot (an agent's slot request waits there). A
   reminder Berth found itself opens without taking the keys; one the user
   asked for comes forward.
   - It names the newest release and the version running, and shows the
     notes of every release since, newest first, as Markdown. Links show as
     their text, and the Claude Code attribution a release ends with is left
     out. Release page opens the newest on GitHub.
   - **Skip This Version** (`settings.update_skipped`): no reminder of it
     again; a newer one gets one, and Check for Updates still shows it.
   - **Later** (esc) closes it until the next look.
   - **Update** (↩) is decision 4.
   - Check for Updates also says when Berth is up to date, or why it couldn't
     look ("GitHub answered 403 Forbidden"), in a small window.
4. **Update downloads, checks and opens; it never installs.**
   - The release's disk image for this Mac's CPU (`Berth-<v>-arm64.dmg`,
     `-x86_64.dmg`) goes to `~/Downloads`, with progress and Cancel. Closing
     the reminder stops it; a stopped or failed download leaves no file.
   - Berth downloads it itself, so it carries no quarantine and Gatekeeper
     won't look at it. Berth checks it instead: its SHA-256 against the
     `digest` GitHub lists for the asset, then `spctl` must accept it as
     notarized for a Developer ID, and when the running Berth is signed, the
     image's Team ID must be the same.
   - Then `open` mounts it: Finder shows Berth beside Applications. The
     reminder says to quit Berth and drag the new one over, with Quit Berth
     and Show in Finder.

## Consequences

- No protocol change, no migration: three settings keys (`update_check`,
  `update_skipped`, `update_checked_at`).
- §13 still defers installing in place. A user drags the new app over the old
  one, as from the release page.
- A release must keep attaching both images under `scripts/dmg.sh`'s names, or
  Update says there is no image for this Mac and points to the release page.
- GitHub allows 60 unauthenticated requests an hour per address; two a day,
  and those the user asks for, stay well under it.
