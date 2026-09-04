---
description: Add a colleague's peer file (pasted JSON or a path) to your global contacts
argument-hint: "<peer json | path/to/peer.json>"
allowed-tools: Bash(owl contact:*), Bash(owl allow:*), Read, Write
---

Arguments: "$ARGUMENTS"

The argument is either a path to a peer file or the peer JSON pasted inline (the output of the
colleague's `owl contact export` / `/owlpost:me`). It must contain `name` and `pubkey`
(`ed25519:<base64>`); `emails` and `endpoints` are optional lists. If a field is missing or
the JSON does not parse, say which and stop.

1. Run `owl contact list --json` and stop with a message if a contact with the same `pubkey`
   already exists.
2. Write the JSON to `$OWLPOST_HOME/contacts/<slug>.json` (default
   `~/.config/owlpost/contacts/`), `<slug>` = the name lowercased, non-alphanumerics replaced
   by `-`. Keep only `name`, `pubkey`, `emails`, `endpoints`.
3. Run `owl contact show <name>` and show the name and fingerprint.
4. Remind the user to confirm the fingerprint with the colleague out-of-band before
   `owl allow <name>`; offer to run it once they confirm.

Note: this writes the file directly until `owl add <file>` is implemented (OWL-019); then
this command becomes a wrapper around it.
