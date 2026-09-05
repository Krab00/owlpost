---
description: Uninstall the owl daemon service
allowed-tools: Bash(owl uninstall:*)
---

Uninstalling stops the daemon, so questions and answers stop flowing until it is installed
again; it needs one confirmation.

1. Tell the user that.
2. Ask for explicit confirmation with AskUserQuestion before running. Do not run anything
   until the user says yes.
3. Run `owl uninstall` and show its output. On a non-zero exit, show the error line and
   stop.
4. Mention that `/owlpost:install` puts the service back.
