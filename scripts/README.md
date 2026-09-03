# scripts

- `opencode-agent.json` — the `owl-readonly` opencode agent used by the `opencode` harness
  (`opencode run --format json --agent owl-readonly`, design §3/§12). Install by merging the
  `agent` object into your opencode config (`~/.config/opencode/opencode.json`, or a project
  `opencode.json`); opencode also accepts it as a separate file when placed there. Verify with
  `opencode run --agent owl-readonly "hello"`.
- `e2e-real.sh` — the OWL-014 manual loop (ask → consent → allow → draft → send → pull →
  show → cache hit) against a real harness: `OWL_HARNESS=claude|codex|opencode
  scripts/e2e-real.sh`. Logs under `target/e2e-real/<timestamp>/`; not run by CI or
  `cargo test` (design §12).
