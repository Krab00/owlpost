# Pilot guide

How to run the 4–5 person pilot from `docs/concept.md` "Rollout": who does what to get on,
how an admin protects the peer list, what to measure, and when to stop. The engineering
contracts stay in `docs/technical-design.md`; this file is the operational checklist.

## Onboarding

One row per pilot user; tick the boxes in the pilot's tracking issue. Every step is a command
from the README quickstart, so a user who gets stuck can be walked through it in a minute.

| # | Step | Command / action | Done when |
|---|---|---|---|
| 1 | Install `owl` | `curl -fsSL https://raw.githubusercontent.com/Krab00/owlpost/main/scripts/install.sh \| sh` | `owl --version` prints the pilot release version |
| 2 | Create identity | `owl init --name "<Name>" --email <work email>` | fingerprint `owl:…` printed; the user pastes it into the tracking issue |
| 3 | Run the daemon as a service | `owl install` (`owl install --dry-run` to see the unit first) | `owl doctor` reports the daemon reachable after a re-login |
| 4 | Export the peer file | `owl contact export > .agents/peers/<slug>.json` in the pilot repository | file contains the same fingerprint as step 2 |
| 5 | Open the PR | PR adding only `.agents/peers/<slug>.json` | approved by an admin after the fingerprint check below, merged |
| 6 | Check the setup | `owl doctor` in the pilot repository after pulling `main` | every line green; every other pilot user is listed as resolvable |
| 7 | First ask | `owl ask <peer> <path> "<question>"` to another pilot user, who runs `owl inbox`, `owl allow <asker>`, `owl draft <id>`, `owl send <id>` | the answer shows up in `owl inbox` on the asker's side and cites a path |
| 8 | Harness plugin | Claude Code users: install the plugin per `plugins/claude-code/README.md` (`/plugin marketplace add`, `/plugin install owlpost@owlpost-local`) | the session shows the `owlpost: N new questions` line when a question is waiting |

Ask each user to keep `owl allow <peer> --always` for at most two trusted peers during the
pilot, so both the human-gated and the automatic path get exercised (`docs/concept.md` MVP
scope, item 5).

## CODEOWNERS and fingerprint check

The repository provider trusts whatever is committed under `.agents/peers/`
(`docs/architecture.md` §5, "Contact list integrity (repo)"). Two controls keep that honest:

1. The directory is owned by the pilot admins. Add to the repository's `CODEOWNERS`:

   ```
   /.agents/peers/  @<org>/<admins-team>
   ```

   With branch protection requiring code-owner review, no peer file lands without an admin
   approving.

2. Before approving a peer-file PR, the admin compares the fingerprint in the PR with one read
   out-of-band:

   - Ask the user to run `owl whoami` on their machine and read the fingerprint over a call,
     chat DM or in person — not in the PR itself.
   - Open the PR's `.agents/peers/<slug>.json` and compare the `fingerprint` field character
     by character (`owl:` plus 16 base32 characters).
   - Check that `email` is the user's work address and that the PR only adds or changes that
     one file.
   - A changed fingerprint on an existing file (new key) is treated as a new onboarding: the
     same call, and the old key is revoked by merging the change.

   Mismatch or no out-of-band confirmation means the PR is not approved. Signed peer-file
   changes are post-MVP; until then, the admin's eyes are the signature.

## Metrics

Collect weekly from every pilot machine with `owl history --json` (finished exchanges from
`done/`; one row per record with `id`, `peer_name`, `type`, `state`, `path`, `received_at`,
`done_at`, `text`). Ask each user to run it and paste the output into the tracking issue, or
have a script gather it. Report per person and in total:

| Metric | From | Why it matters |
|---|---|---|
| Asks per person per week | rows with `type == "question"` on the asker's side, grouped by `peer_name` | shows whether people reach for it at all |
| Answer latency | `done_at - received_at` on the responder's side, split by policy: manual (`owl allow`) vs automatic (`--always`) | the human gate is the cost of the trust model; the pilot must measure both |
| Answered vs denied/rejected | `state` of each question record: `sent` vs `rejected`, plus `owl deny` policies | a high reject rate means questions are badly targeted or owners do not want to answer |
| Cache hits | asker-side `owl ask` output `cached answer` vs new `accepted <id>` | repeated questions should not cost the responder anything |
| Answers that cite a path | answer `text` containing a repository path | an answer without a path is usually no better than a guess |
| Notes opted in | responders whose project notes are readable by the runner | the delta over git history depends on this (see below) |

Also record qualitatively, once a week per user: one question that got a better answer than
`git blame` / PR history would have given, and one that did not.

## Kill criteria

From `docs/concept.md` "Rollout": if the pilot does not show value in about two weeks,
adoption will fail regardless of code quality. Stop the pilot (and rethink, not polish) when
any of these holds at the two-week review:

- **No value in ~2 weeks**: fewer than one real ask per person per week in the second week, or
  users say they would not miss it.
- **Answers do not beat git history**: in the weekly samples the answers are not better than
  what an agent extracts itself from `git blame`, PR descriptions and issues.
- **Owners do not opt their notes in**: the unique value is the delta — private notes and
  context absent from repo history — and if responders keep their notes out of the runner's
  reach there is no delta to measure.
- **Trust model rejected**: responders leave questions unanswered because approving each one
  is too much friction and they refuse `--always` for anyone.

If none holds, the company-wide rollout follows with the same one-command install and the
same CODEOWNERS procedure per repository.
