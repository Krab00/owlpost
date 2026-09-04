//! OWL-015 AC3/AC4: the README quickstart and the pilot guide are data deliverables; these
//! tests pin the markers the acceptance criteria name so drift is caught by `cargo test`.

use std::fs;

fn readme() -> String {
    fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md")).unwrap()
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
