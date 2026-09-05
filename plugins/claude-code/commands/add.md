---
description: Add a colleague's peer file (pasted JSON or a path) to your global contacts
argument-hint: "<peer json | path/to/peer.json> [--local]"
allowed-tools: Bash(owl add:*), Bash(owl contact:*), Bash(owl allow:*), Read
---

Arguments: "$ARGUMENTS"

The argument is either a path to a peer file or the peer JSON pasted inline (the output of the
colleague's `owl contact export` / `/owlpost:me`). A trailing `--local` means the contact
goes into this repository's `.agents/peers/` (shared with the team via PR) instead of the
global book in `$OWLPOST_HOME/contacts/`.

1. Run `owl add` with the argument: a path as `owl add <path>`, pasted JSON via stdin as
   `owl add -` (pass the JSON exactly as pasted). Append `--local` when asked for. `owl add`
   validates `name`, `pubkey`, `emails`, `endpoints`, refuses a pubkey that is already a
   contact in either scope, and prints `added <name> <fingerprint> (global|local)`.
2. On a non-zero exit, show the error line (it names the missing or invalid field, or the
   existing contact) and stop.
3. Show the user the name and fingerprint from the `added` line.
4. Remind the user to confirm the fingerprint with the colleague out-of-band before
   `owl allow <name>`; offer to run it once they confirm.
