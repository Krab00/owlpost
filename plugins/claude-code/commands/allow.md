---
description: Allow a peer — release held questions and set policy manual (--once releases without a policy, --always sets auto)
argument-hint: "<peer> [--once | --always [--i-verified-the-fingerprint]]"
allowed-tools: Bash(owl allow:*)
---

Arguments: "$ARGUMENTS"

Allowing changes who may ask this machine questions, so it needs one confirmation.

1. Tell the user what will happen: `owl allow <peer>` releases the peer's held questions and
   sets policy `manual`; `--once` releases them without setting a policy; `--always` sets
   `auto` (a hand-added contact additionally needs `--i-verified-the-fingerprint`, which the
   user must confirm they did out-of-band).
2. `--always` takes the fingerprint and nothing else: a name prefix or an e-mail exits 2 with
   `owl allow --always needs the fingerprint, not a name: verify it out-of-band and pass owl:… (this peer: <fp>)`,
   writing no policy and releasing nothing. Pass the `owl:…` from that message only after the
   user says they checked it out-of-band. `owl allow <peer>` and `--once` still take a name.
3. Ask for explicit confirmation with AskUserQuestion before running. Do not run anything
   until the user says yes.
4. Run `owl allow $ARGUMENTS` and show the output. On a non-zero exit, show the error line
   (unknown peer, or a missing `--i-verified-the-fingerprint`) and stop.
5. Offer `/owlpost:inbox` to see the released questions.
