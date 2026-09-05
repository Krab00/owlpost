---
description: Show this machine's owlpost identity summary (name, email, fingerprint, home)
allowed-tools: Bash(owl whoami:*)
---

Run `owl whoami` and show its output verbatim. If it fails because the home is not
initialised, say so and offer `/owlpost:init`. To hand the identity to a colleague as a peer
file, point the user to `/owlpost:me` instead.
