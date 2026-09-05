---
description: List the contact book as a table and show how to ask a contact by mentioning it (@owl:to://…)
allowed-tools: Bash(owl contact:*)
---

No arguments. This command only lists; a contact is asked by mentioning it in the prompt.

1. Run `owl contact list --json`. It prints a JSON array with one object per contact of the
   merged book (global `$OWLPOST_HOME/contacts/` plus this repository's `.agents/peers/`):
   `name`, `fingerprint`, `source` (`global` or `local`), `emails`, `endpoints` and, only
   when a policy is set, `policy` with `mode` (`manual`, `auto` or `never`).
   If the array is empty, print one line: "No contacts yet — run /owlpost:add with a colleague's peer file." and stop.
2. Print the book as ONE table, sorted by name, with the columns name, e-mails, fingerprint,
   source and policy; a contact without `policy` shows `-` in the policy column.
3. End with exactly this hint, `<first contact's uri>` being the first row's resource URI as
   `owl mcp` serves it: the name lower-cased with every run of non-alphanumerics as one `-`,
   then `.` and the first e-mail (no e-mail: the slug alone), for example
   `ana-kowalska.ana@acme.pl`:
   Type @ and the contact's name, e.g. @owl:to://<first contact's uri>, then the question.
