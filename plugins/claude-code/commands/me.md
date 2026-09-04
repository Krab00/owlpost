---
description: Show this machine's owlpost identity (peer file) to send to a colleague
allowed-tools: Bash(owl contact export:*), Bash(owl doctor:*)
---

Run `owl contact export` and show the JSON in a code block. Tell the user to send it to the
colleague, who saves it as a file and runs `owl add <file>`, then confirms the fingerprint
out-of-band before `owl allow`.
