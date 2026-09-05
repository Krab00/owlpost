---
description: Check key, config, endpoints, harnesses and daemon; explain any failure and offer the fix
allowed-tools: Bash(owl doctor:*)
---

Run `owl doctor` and show its output verbatim. Every line is one check; a failed check names
what is missing.

For each failure offer the fix: no home or key → `/owlpost:init`; daemon not running →
`/owlpost:install`; a stale binary → `/owlpost:update`; no contacts → `/owlpost:add`; an
unreachable endpoint or a missing harness → the config change the line names. Do not run
any fix on your own.
