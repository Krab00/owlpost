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
/// OWL-026 AC2 and the doc quotes of the new injection prefix: `/owlpost:watch status` arms
/// the watch when the default is on and none runs (one line in `commands/watch.md`, pinned
/// whole); the plugin README, the skill and the design doc name the sentence by its prefix.
#[test]
fn watch_status_arms_and_docs_quote_the_arm_prefix() {
    let watch = repo_file("plugins/claude-code/commands/watch.md");
    let status = watch
        .split("## `status` (or no argument)")
        .nth(1)
        .and_then(|s| s.split("\n## ").next())
        .expect("watch.md status section");
    assert!(
        status.lines().any(|l| l == "4. When no \"owlpost inbox\" watch runs in this session and the stored default is on, arm it exactly as `on` does (same `Monitor` call, the command from this turn's sentence) and say `owlpost watch: running; default on` instead."),
        "watch.md status lacks the arming line"
    );
    for (rel, needle) in [
        (
            "plugins/claude-code/README.md",
            "`status` reports both and arms the watch when the default is on and none runs; the `SessionStart` and `UserPromptSubmit` hooks inject a self-contained arm instruction starting `owlpost: before handling this prompt` whenever no watch is live for the session (a running watch leaves its pid in `$OWLPOST_HOME/watch/<session id>`)",
        ),
        (
            "plugins/claude-code/skills/owlpost/SKILL.md",
            "`/owlpost:watch status` arms the watch when the default is on and none runs in this session.",
        ),
        (
            "plugins/claude-code/skills/owlpost/SKILL.md",
            "whenever it is present, arm",
        ),
        (
            "plugins/claude-code/commands/watch.md",
            "it arms on your next prompt",
        ),
        (
            "docs/technical-design.md",
            "sentence starting `owlpost: before handling this prompt` (OWL-026: a self-contained",
        ),
        (
            "docs/technical-design.md",
            "the template byte-identical to `commands/watch.md`",
        ),
        // OWL-029 AC5: §9 describes the marker and the per-turn re-arm, §11 the update.
        (
            "docs/technical-design.md",
            "the sentence goes out when no live marker `$OWLPOST_HOME/watch/<session id>` exists",
        ),
        (
            "docs/technical-design.md",
            "so a watch that was never armed or died is asked for again on the next prompt",
        ),
        ("docs/technical-design.md", "never on `PostToolUse`."),
        (
            "docs/technical-design.md",
            "`--count --follow` (plain only) is the live watch: with `--session <id>` (`[A-Za-z0-9._-]{1,128}`",
        ),
        (
            "docs/technical-design.md",
            "`ok binary: <path>` when the first `owl` on `PATH`, canonicalised, is the running file",
        ),
        (
            "docs/technical-design.md",
            "`rename`s it over the active file — a plain copy fails with",
        ),
        (
            "docs/technical-design.md",
            "`installed <active>`; a mismatch is a hard error naming both paths",
        ),
        ("docs/technical-design.md", "`would replace <active>`"),
    ] {
        assert!(repo_file(rel).contains(needle), "{rel} lacks {needle:?}");
    }
    for (rel, gone) in [
        (
            "docs/technical-design.md",
            "--session-start` (the `SessionStart` hook)",
        ),
        ("docs/technical-design.md", "poll script one-liner"),
        ("plugins/claude-code/hooks/hooks.json", "--session-start"),
    ] {
        assert!(!repo_file(rel).contains(gone), "{rel} still says {gone:?}");
    }
    assert!(
        !repo_file("docs/technical-design.md").contains("arm the inbox watch (see /owlpost:watch)"),
        "design doc still quotes the OWL-023 pointer sentence"
    );
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

/// OWL-029 AC7 (data guard): the real-harness proof script exists, is executable, is plain
/// `sh`, and names the three first prompts, the evidence knobs and the bounds.
#[test]
fn e2e_watch_arm_script_is_executable_and_names_the_three_prompts() {
    use std::os::unix::fs::PermissionsExt;
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/e2e-watch-arm.sh");
    let mode = fs::metadata(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .permissions()
        .mode();
    assert_ne!(mode & 0o111, 0, "e2e-watch-arm.sh must be executable");
    let s = fs::read_to_string(&path).unwrap();
    assert!(s.starts_with("#!/bin/sh\n"));
    for needle in [
        "/owlpost:watch status",
        "/owlpost:whoami",
        "what is 2+2? answer with the number only",
        "--output-format stream-json",
        "--verbose",
        "--max-turns 6",
        "timeout 180",
        "E2E_PLUGIN_DIR",
        "E2E_OUT",
        "--plugin-dir",
        "owl init",
        "\"description\":\"owlpost inbox\"",
        "\"name\":\"Monitor\"",
        "PASS ",
        "FAIL ",
    ] {
        assert!(s.contains(needle), "e2e-watch-arm.sh lacks {needle:?}");
    }
    let design = repo_file("docs/technical-design.md");
    assert!(design.contains("`scripts/e2e-watch-arm.sh` (OWL-029)"));
}
