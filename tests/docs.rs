//! OWL-015 AC3/AC4: the README quickstart and the pilot guide are data deliverables; these
//! tests pin the markers the acceptance criteria name so drift is caught by `cargo test`.

use std::fs;
use std::path::Path;

fn readme() -> String {
    fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md")).unwrap()
}

fn repo_file(rel: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel))
        .unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn pilot() -> String {
    fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/pilot.md"))
        .expect("docs/pilot.md exists")
}

/// Byte offset of `needle` in `hay` after `from`, or a panic naming the missing marker.
fn find_after(hay: &str, needle: &str, from: usize) -> usize {
    hay[from..]
        .find(needle)
        .map(|i| from + i + needle.len())
        .unwrap_or_else(|| panic!("{needle:?} not found after byte {from}"))
}

#[test]
fn readme_quickstart_steps_in_order() {
    let s = readme();
    let start = s.find("\n## Quickstart\n").expect("## Quickstart heading");
    // The section ends at the next heading (or the end of the file).
    let end = s[start + 1..]
        .find("\n## ")
        .map_or(s.len(), |i| start + 1 + i);
    let q = &s[start..end];

    let markers = [
        "curl -fsSL https://raw.githubusercontent.com/Krab00/owlpost/main/scripts/install.sh | sh",
        "`owl init`",
        "`owl install`",
        "`owl contact export`",
        "PR",
        "`owl doctor`",
        "`owl ask",
    ];
    let mut at = 0;
    for m in markers {
        at = find_after(q, m, at);
    }
    // The numbered steps are exactly 1..=7 in order.
    let numbers: Vec<&str> = q
        .lines()
        .filter(|l| l.len() > 2 && l.as_bytes()[0].is_ascii_digit() && l[1..].starts_with(". "))
        .map(|l| &l[..1])
        .collect();
    assert_eq!(numbers, ["1", "2", "3", "4", "5", "6", "7"]);
    // Step 5 is the PR into the repo provider directory.
    let step5 = q.lines().find(|l| l.starts_with("5. ")).unwrap();
    assert!(
        step5.contains("PR") && step5.contains(".agents/peers/"),
        "{step5}"
    );
    assert!(
        q.contains("docs/pilot.md"),
        "quickstart points pilot users to the guide"
    );
}

#[test]
fn readme_install_line_matches_the_script_header() {
    let script =
        fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/install.sh")).unwrap();
    let line =
        "curl -fsSL https://raw.githubusercontent.com/Krab00/owlpost/main/scripts/install.sh | sh";
    assert!(readme().contains(line));
    assert!(
        script.contains(line),
        "install.sh header documents the same one-liner"
    );
}

#[test]
fn pilot_guide_has_the_four_sections_in_order() {
    let s = pilot();
    let headings: Vec<&str> = s.lines().filter_map(|l| l.strip_prefix("## ")).collect();
    assert_eq!(
        headings,
        [
            "Onboarding",
            "CODEOWNERS and fingerprint check",
            "Metrics",
            "Kill criteria"
        ]
    );
    assert!(s.starts_with("# Pilot guide\n"));
}

#[test]
fn pilot_sections_carry_their_required_content() {
    let s = pilot();
    let section = |name: &str| -> String {
        let start = s.find(&format!("\n## {name}\n")).unwrap();
        let body = &s[start + 1..];
        let end = body[1..].find("\n## ").map_or(body.len(), |i| i + 1);
        body[..end].to_string()
    };

    let on = section("Onboarding");
    for step in [
        "scripts/install.sh",
        "owl init",
        "owl install",
        "owl contact export",
        ".agents/peers/",
        "owl doctor",
        "owl ask",
        "plugins/claude-code/README.md",
    ] {
        assert!(on.contains(step), "Onboarding lacks {step}");
    }

    let co = section("CODEOWNERS and fingerprint check");
    assert!(co.contains("/.agents/peers/"), "the CODEOWNERS line");
    assert!(co.contains("owl whoami"), "out-of-band fingerprint source");
    assert!(co.contains("out-of-band"), "{co}");

    let me = section("Metrics");
    assert!(me.contains("owl history --json"));
    for m in [
        "Asks per person",
        "latency",
        "denied/rejected",
        "Cache hits",
        "cite a path",
    ] {
        assert!(me.contains(m), "Metrics lacks {m}");
    }

    let kill = section("Kill criteria");
    for k in ["2 weeks", "git blame", "notes"] {
        assert!(kill.contains(k), "Kill criteria lacks {k}");
    }
}

/// OWL-028 AC4: the README's answer paragraph names the one-pick `Draft & send`.
#[test]
fn readme_answer_paragraph_names_draft_and_send() {
    let s = readme();
    let intro = &s[..s.find("\n## Quickstart\n").expect("## Quickstart heading")];
    assert!(
        intro.contains("approves — one `Draft & send` pick in `/owlpost:inbox` — or automatically"),
        "README intro lacks the Draft & send mention"
    );
}

/// OWL-027 AC4: one README line describes the owl icon on the counter and the orange frame
/// around a peer's message.
#[test]
fn readme_describes_the_owl_icon_and_orange_frame() {
    let s = readme();
    let line = s
        .lines()
        .find(|l| l.contains("🦉") && l.contains("🟧"))
        .expect("README line with the owl icon and the orange frame");
    for needle in [
        "counter line wears an owl",
        "`🦉 owlpost: 1 new answer ...`",
        "orange `🟧` frame",
        "drafts are not framed",
    ] {
        assert!(
            line.contains(needle),
            "README frame line lacks {needle:?}: {line}"
        );
    }
}
/// Every regular file under `dir`, recursively.
fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

/// OWL-031 AC5: the plugin docs, the design doc and the README describe the event-driven
/// wake (`watchPaths`, `FileChanged`, `asyncRewake`, exit 2, the `add`-only rule, the
/// dead-marker sweep) and nothing of the Monitor era is left — each literal pinned on one
/// line, each retired phrase asserted absent over `plugins/claude-code/`, `docs/` and
/// `README.md`.
#[test]
fn watch_docs_describe_the_event_driven_wake_and_drop_the_arm_sentence() {
    let design = repo_file("docs/technical-design.md");
    let section = |name: &str| -> String {
        let start = design
            .find(&format!("\n## {name}"))
            .unwrap_or_else(|| panic!("§{name}"));
        let body = &design[start + 1..];
        let end = body[1..].find("\n## ").map_or(body.len(), |i| i + 1);
        body[..end].to_string()
    };
    let s9 = section("9. CLI contract");
    for needle in [
        "`watchPaths`",
        "`FileChanged`",
        "`asyncRewake`",
        "exit 2",
        "carries `watchPaths: [\"<home>/spool/inbox\"]`",
        "(absolute; `<home>` is the resolved `$OWLPOST_HOME`, canonicalised when it exists) next to",
        "`additionalContext` is present only when there is text",
        "When `$OWLPOST_HOME/plugin.json` reads `{\"watch\": false}` the `watchPaths`",
        "key is absent and count 0 prints nothing",
        "`UserPromptSubmit` and `PostToolUse` never carry `watchPaths`",
        "`SessionStart` also sweeps `$OWLPOST_HOME/watch/`: every marker whose pid is dead is removed",
        "(all session ids), live ones stay, and an absent or unreadable directory never fails the hook.",
        "is `add`, the unseen count is above 0 and the watch is on, prints the counter sentence",
        "(`sentence()`, 🦉 icon, byte-identical to `--follow`) to stderr and exits `2` — the exit",
        "code Claude Code's `asyncRewake` hook turns into a new model turn, showing the hook's stderr",
        "Keying on `add` only is the de-duplication:",
        "marking seen and moving to `done/` are `change`/`unlink`, so they never wake",
        "`--format plain|codex|kimi` (or no `--format`) is a clap usage error (exit 2, distinct from the wake by its",
        "`--count --follow` (plain only) is the poll-loop fallback for hosts without a `FileChanged` hook",
    ] {
        assert!(
            s9.lines().any(|l| l.contains(needle)),
            "design §9 lacks {needle:?} on one line"
        );
    }
    let s11 = section("11. Notifications and service install");
    for needle in [
        "- Claude Code live watch (OWL-031): the plugin's `SessionStart` hook returns `watchPaths`",
        "with the spool inbox directory and its `FileChanged` hook runs with `asyncRewake: true`",
        "exits 2 with the counter sentence when a record is added, which wakes the idle session with",
        "stays as the poll-loop fallback for hosts without such a hook; its marker under",
        "`$OWLPOST_HOME/watch/` is informational and dead ones are swept on `SessionStart`.",
    ] {
        assert!(
            s11.lines().any(|l| l.contains(needle)),
            "design §11 lacks {needle:?} on one line"
        );
    }
    let s12 = section("12. Testing strategy");
    for needle in [
        "`scripts/e2e-watch-wake.sh` (OWL-031)",
        "waits for the first `result` event, drops one unseen question record into `spool/inbox/`",
        "`hook_response` for `FileChanged` with `exit_code` 2 and the 🦉 sentence followed by a second `assistant` event with no second user message sent",
    ] {
        assert!(
            s12.lines().any(|l| l.contains(needle)),
            "design §12 lacks {needle:?} on one line"
        );
    }
    assert!(
        design.lines().any(|l| l.contains(
            "e2e-watch-wake.sh  one `claude -p` stream-json session; proves the FileChanged hook wakes it (exit 2) when a record lands"
        )),
        "design §2 scripts row"
    );
    for (rel, needle) in [
        (
            "plugins/claude-code/skills/owlpost/SKILL.md",
            "nothing to arm",
        ),
        ("plugins/claude-code/README.md", "nothing to arm"),
        (
            "plugins/claude-code/commands/watch.md",
            "`owlpost watch: on; the inbox is watched from the next session start`",
        ),
        (
            "plugins/claude-code/commands/watch.md",
            "`owlpost watch: off; this session stops waking now, new sessions do not watch`",
        ),
        (
            "plugins/claude-code/commands/watch.md",
            "`owlpost watch: default on|off; event-driven (FileChanged), nothing to arm`",
        ),
    ] {
        assert!(
            repo_file(rel).lines().any(|l| l.contains(needle)),
            "{rel} lacks {needle:?} on one line"
        );
    }
    // The README `/owlpost:watch` row and the SKILL "Live watch" section carry the phrase.
    let row = repo_file("plugins/claude-code/README.md")
        .lines()
        .find(|l| l.contains("`/owlpost:watch [on\\|off\\|status]`"))
        .expect("README /owlpost:watch row")
        .to_string();
    assert!(row.contains("nothing to arm"), "{row}");
    let skill = repo_file("plugins/claude-code/skills/owlpost/SKILL.md");
    let live = skill
        .split("\n## Live watch\n")
        .nth(1)
        .and_then(|s| s.split("\n## ").next())
        .expect("SKILL.md Live watch section");
    assert!(live.contains("nothing to arm"), "{live}");
    // The retired phrases are gone from every file the AC names.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = walk(&root.join("plugins/claude-code"));
    files.extend(walk(&root.join("docs")));
    files.push(root.join("README.md"));
    for path in files {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        for gone in [
            "owlpost: before handling this prompt",
            "arm the inbox watch",
            "it arms on your next prompt",
            "e2e-watch-arm",
        ] {
            assert!(
                !text.contains(gone),
                "{} still contains {gone:?}",
                path.display()
            );
        }
        if path.starts_with(root.join("plugins/claude-code")) {
            assert!(
                !text.contains("Monitor"),
                "{} still says Monitor",
                path.display()
            );
        }
    }
    for gone in [
        "--session-start` (the `SessionStart` hook)",
        "poll script one-liner",
        "the template byte-identical to `commands/watch.md`",
        "so a watch that was never armed or died is asked for again on the next prompt",
        "OWL-029 per-turn re-arm",
    ] {
        assert!(!design.contains(gone), "design doc still says {gone:?}");
    }
    assert!(!repo_file("plugins/claude-code/hooks/hooks.json").contains("--session-start"));
}

/// OWL-029 AC5/AC6 doc pins that survive OWL-031: the `owl update` and `owl doctor` binary
/// sentences in the design doc.
#[test]
fn update_and_doctor_docs_name_the_binary_check_and_the_rename() {
    for needle in [
        "`ok binary: <path>` when the first `owl` on `PATH`, canonicalised, is the running file",
        "`rename`s it over the active file — a plain copy fails with",
        "`installed <active>`; a mismatch is a hard error naming both paths",
        "`would replace <active>`",
    ] {
        assert!(
            repo_file("docs/technical-design.md").contains(needle),
            "docs/technical-design.md lacks {needle:?}"
        );
    }
}

/// OWL-030 AC4: the design doc says the unit is rewritten from the path captured before the
/// replacement (and why), the README quickstart names the one-time bootstrap for installs
/// whose `owl update` still targets `~/.cargo/bin`.
#[test]
fn update_docs_name_the_captured_path_and_the_bootstrap() {
    for (rel, needle) in [
        (
            "docs/technical-design.md",
            "The unit is rewritten from the path captured before the replacement (OWL-030): on Linux a",
        ),
        (
            "docs/technical-design.md",
            "running process's own path (`/proc/self/exe`) reads `<path> (deleted)` after a rename over",
        ),
        (
            "docs/technical-design.md",
            "it, and `install::owl_path()` strips that suffix before canonicalising, so `owl doctor` and",
        ),
        (
            "README.md",
            "`cp ~/.cargo/bin/owl ~/.local/bin/owl.new && mv ~/.local/bin/owl.new ~/.local/bin/owl`",
        ),
        (
            "README.md",
            "(installed by `cargo install`) still updates into `~/.cargo/bin`: bootstrap once with",
        ),
    ] {
        assert!(
            repo_file(rel).lines().any(|l| l.contains(needle)),
            "{rel} lacks {needle:?} on one line"
        );
    }
}

/// OWL-031 AC6 (data guard): the real-harness proof script exists, is executable, is plain
/// `sh`, uses the stream-json input, drops the record only after the first `result` event
/// and asserts the `FileChanged` wake followed by a second assistant event; the old
/// `e2e-watch-arm.sh` is gone.
#[test]
fn e2e_watch_wake_script_is_executable_and_pins_the_flow() {
    use std::os::unix::fs::PermissionsExt;
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(
        !root.join("scripts/e2e-watch-arm.sh").exists(),
        "scripts/e2e-watch-arm.sh must be deleted"
    );
    let path = root.join("scripts/e2e-watch-wake.sh");
    let mode = fs::metadata(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .permissions()
        .mode();
    assert_ne!(mode & 0o111, 0, "e2e-watch-wake.sh must be executable");
    let s = fs::read_to_string(&path).unwrap();
    assert!(s.starts_with("#!/bin/sh\n"));
    for needle in [
        "--input-format stream-json",
        "--output-format stream-json",
        "--verbose",
        "--include-hook-events",
        "--permission-mode bypassPermissions",
        "--plugin-dir",
        "E2E_PLUGIN_DIR",
        "E2E_OUT",
        "owl init",
        "timeout 180",
        "Reply with exactly the word: ready",
        "\"type\":\"result\"",
        "\"hook_response\"",
        "\"hook_event\":\"FileChanged\"",
        "\"exit_code\":2",
        "owlpost: 1 new question",
        "\"type\":\"assistant\"",
        "spool/inbox/",
        "echo \"PASS",
        "echo \"FAIL $reason\"",
        "exit 1",
    ] {
        assert!(s.contains(needle), "e2e-watch-wake.sh lacks {needle:?}");
    }
    // The pins below are on the code, not the header comment: comment lines are dropped.
    let code: String = s
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .map(|l| format!("{l}\n"))
        .collect();
    // Both `claude -p` invocations (with and without --plugin-dir) take stream-json input.
    assert_eq!(
        code.matches("--input-format stream-json --output-format stream-json --verbose")
            .count(),
        2,
        "both claude invocations use --input-format stream-json"
    );
    assert!(!code.contains("--input-format text"), "{code}");
    // The wake check greps the hook_response for exit code 2 and the FileChanged event.
    assert!(
        code.contains(
            "grep -F '\"hook_event\":\"FileChanged\"' | grep -F '\"exit_code\":2' | grep -F 'owlpost: 1 new question'"
        ),
        "the wake check pins hook_event FileChanged, exit_code 2 and the sentence"
    );
    // Order: one user message is sent, the first result event is awaited (and its absence
    // fails the run), only then is the record dropped into spool/inbox/ — exactly once — and
    // then the wake is asserted.
    let drop_cmd = "mv \"$WORK/$id.json\" \"$HOME_DIR/spool/inbox/$id.json\"";
    assert_eq!(
        code.matches(drop_cmd).count(),
        1,
        "one drop into spool/inbox"
    );
    let send = code.find("Reply with exactly the word: ready").unwrap();
    let wait_result = code[send..]
        .find("result_line=$(grep -n -F '\"type\":\"result\"'")
        .unwrap()
        + send;
    let no_result = code[wait_result..]
        .find("echo \"FAIL no result event within")
        .unwrap()
        + wait_result;
    let drop = code.find(drop_cmd).unwrap();
    let wake = code[drop..].find("\"hook_event\":\"FileChanged\"").unwrap() + drop;
    assert!(
        send < wait_result && wait_result < no_result && no_result < drop && drop < wake,
        "send={send} wait={wait_result} no_result={no_result} drop={drop} wake={wake}"
    );
    // Exactly one user message is ever written to the FIFO.
    assert_eq!(
        code.matches("\"type\":\"user\"").count(),
        1,
        "one user message"
    );
    let design = repo_file("docs/technical-design.md");
    assert!(design.contains("`scripts/e2e-watch-wake.sh` (OWL-031)"));
    assert!(!design.contains("e2e-watch-arm"));
}
