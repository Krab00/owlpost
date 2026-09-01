use std::path::{Path, PathBuf};

use owlpost::contacts::{ContactBook, Mode, Policy, Scope};
use owlpost::identity::{Identity, fingerprint, pubkey_string};

fn pk(seed: u8) -> String {
    pubkey_string(&Identity::from_seed([seed; 32]).verifying_key())
}

fn fp(seed: u8) -> String {
    fingerprint(&Identity::from_seed([seed; 32]).verifying_key())
}

fn peer_json(name: &str, email: &str, seed: u8) -> String {
    serde_json::json!({
        "name": name,
        "emails": [email],
        "pubkey": pk(seed),
        "endpoints": [format!("{}.example.org:7411", name.to_lowercase())],
    })
    .to_string()
}

/// Fixture git repo: `<root>/.git/`, `.agents/peers/{maciek,marek}.json`, nested `src/deep/`.
/// Returns (root, nested subdir).
fn fixture_repo(base: &Path) -> (PathBuf, PathBuf) {
    let root = base.join("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let peers = root.join(".agents").join("peers");
    std::fs::create_dir_all(&peers).unwrap();
    std::fs::write(
        peers.join("maciek.json"),
        peer_json("Maciek", "maciek@company.com", 1),
    )
    .unwrap();
    std::fs::write(
        peers.join("marek.json"),
        peer_json("Marek", "marek@company.com", 2),
    )
    .unwrap();
    let sub = root.join("src").join("deep");
    std::fs::create_dir_all(&sub).unwrap();
    (root, sub)
}

fn overlay(home: &Path, seed: u8, name: &str, mode: &str) {
    let dir = home.join("contacts");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{}.json", fp(seed))),
        serde_json::json!({
            "name": name,
            "emails": ["other@example.org"],
            "pubkey": pk(seed),
            "endpoints": ["other.example.org:1"],
            "source": "local",
            "policy": { "mode": mode, "scope": { "projects": ["*"] } },
            "added_at": "2026-09-01T10:00:00Z",
        })
        .to_string(),
    )
    .unwrap();
}

#[test]
fn repo_provider_finds_peers_from_subdir() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (_root, sub) = fixture_repo(tmp.path());
    let book = ContactBook::load(&home, &sub).unwrap();
    assert_eq!(book.contacts.len(), 2);
    let maciek = book.resolve(&fp(1)).unwrap();
    assert_eq!(maciek.name, "Maciek");
    assert_eq!(maciek.emails, ["maciek@company.com"]);
    assert_eq!(maciek.endpoints, ["maciek.example.org:7411"]);
    assert_eq!(maciek.pubkey, pk(1));
    assert_eq!(maciek.fingerprint(), fp(1));
    assert_eq!(maciek.source, "repo");
    assert!(maciek.policy.is_none());
    let marek = book.resolve(&fp(2)).unwrap();
    assert_eq!(marek.name, "Marek");
    assert_eq!(marek.source, "repo");
    assert!(book.contacts.iter().all(|c| c.source == "repo"));
}

#[test]
fn no_git_root_means_no_repo_contacts() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    // Peers dir exists but there is no `.git` anywhere up the tree of `sub`.
    let (root, sub) = fixture_repo(tmp.path());
    std::fs::remove_dir(root.join(".git")).unwrap();
    // The tempdir itself lives under /tmp (or similar) which is not a git repo.
    let book = ContactBook::load(&home, &sub).unwrap();
    assert!(book.contacts.is_empty(), "{:?}", book.contacts);
}

#[test]
fn local_overlay_only_contributes_policy() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (_root, sub) = fixture_repo(tmp.path());
    overlay(&home, 1, "Wrong Name", "auto");
    let book = ContactBook::load(&home, &sub).unwrap();
    assert_eq!(book.contacts.len(), 2, "overlay must not add a contact");
    let c = book.resolve(&fp(1)).unwrap();
    assert_eq!(c.name, "Maciek");
    assert_eq!(c.emails, ["maciek@company.com"]);
    assert_eq!(c.endpoints, ["maciek.example.org:7411"]);
    assert_eq!(c.source, "repo");
    assert_eq!(c.added_at, None, "overlay's added_at must not leak");
    assert_eq!(c.policy.as_ref().unwrap().mode, Mode::Auto);
    assert_eq!(book.policy_for(&fp(1)).unwrap().mode, Mode::Auto);
    assert!(book.policy_for(&fp(2)).is_none());
    assert!(
        book.resolve("Wrong").is_err(),
        "overlay name must not resolve"
    );
}

#[test]
fn local_only_contact_is_full_contact() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (_root, sub) = fixture_repo(tmp.path());
    overlay(&home, 3, "Ola", "manual");
    let book = ContactBook::load(&home, &sub).unwrap();
    assert_eq!(book.contacts.len(), 3);
    let ola = book.resolve("ola").unwrap();
    assert_eq!(ola.name, "Ola");
    assert_eq!(ola.emails, ["other@example.org"]);
    assert_eq!(ola.endpoints, ["other.example.org:1"]);
    assert_eq!(ola.source, "local");
    assert_eq!(ola.fingerprint(), fp(3));
    assert_eq!(ola.added_at.as_deref(), Some("2026-09-01T10:00:00Z"));
    assert_eq!(ola.policy.as_ref().unwrap().mode, Mode::Manual);
    // Source is decided by the provider, not by the file.
    let dir = home.join("contacts");
    std::fs::write(
        dir.join("lying.json"),
        format!(r#"{{"name":"Liar","pubkey":"{}","source":"repo"}}"#, pk(4)),
    )
    .unwrap();
    let book = ContactBook::load(&home, &sub).unwrap();
    assert_eq!(book.resolve("Liar").unwrap().source, "local");
    // Repo entries are listed before local ones.
    let sources: Vec<&str> = book.contacts.iter().map(|c| c.source.as_str()).collect();
    assert_eq!(sources, ["repo", "repo", "local", "local"]);
}

#[test]
fn resolve_orders_and_ambiguity() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (root, sub) = fixture_repo(tmp.path());
    // A third peer whose *name* equals Maciek's email, to prove email beats name.
    std::fs::write(
        root.join(".agents/peers/z.json"),
        peer_json("maciek@company.com", "z@company.com", 3),
    )
    .unwrap();
    let book = ContactBook::load(&home, &sub).unwrap();
    assert_eq!(book.contacts.len(), 3);
    // by fingerprint
    assert_eq!(book.resolve(&fp(2)).unwrap().name, "Marek");
    // by email (beats the exact-name match of peer 3)
    assert_eq!(
        book.resolve("maciek@company.com").unwrap().fingerprint(),
        fp(1)
    );
    assert_eq!(book.resolve("z@company.com").unwrap().fingerprint(), fp(3));
    // by unique name prefix, case-insensitive
    assert_eq!(book.resolve("mar").unwrap().name, "Marek");
    assert_eq!(book.resolve("MAR").unwrap().name, "Marek");
    // "Maci" is a prefix of both "Maciek" and the peer named "maciek@company.com".
    assert!(book.resolve("Maci").is_err());
    assert_eq!(book.resolve("marek").unwrap().name, "Marek");
    // ambiguous prefix lists both names
    let err = book.resolve("ma").unwrap_err().to_string();
    assert!(err.contains("Maciek"), "{err}");
    assert!(err.contains("Marek"), "{err}");
    assert!(err.contains("maciek@company.com"), "{err}");
    assert!(err.to_lowercase().contains("ambiguous"), "{err}");
    // negatives: wrong fingerprint, wrong email, prefix of nothing, non-prefix substring
    assert!(book.resolve(&fp(9)).is_err());
    assert!(book.resolve("nobody@company.com").is_err());
    assert!(book.resolve("xyz").is_err());
    assert!(book.resolve("arek").is_err(), "substring is not a prefix");
    assert!(book.resolve(&fp(1)[..10]).is_err(), "partial fingerprint");
}

#[test]
fn resolve_unknown_is_error() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (_root, sub) = fixture_repo(tmp.path());
    let book = ContactBook::load(&home, &sub).unwrap();
    let err = book.resolve("nobody").unwrap_err().to_string();
    assert!(err.contains("nobody"), "{err}");
    let empty = ContactBook::default();
    assert!(empty.resolve("").is_err());
    assert!(empty.resolve(&fp(1)).is_err());
}

#[test]
fn malformed_file_is_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (root, sub) = fixture_repo(tmp.path());
    let peers = root.join(".agents/peers");
    std::fs::write(peers.join("broken.json"), "{not json").unwrap();
    std::fs::write(
        peers.join("badkey.json"),
        r#"{"name":"Bad","pubkey":"ed25519:AAAA"}"#,
    )
    .unwrap();
    std::fs::write(peers.join("nokey.json"), r#"{"name":"NoKey"}"#).unwrap();
    std::fs::write(peers.join("README.md"), "not a contact").unwrap();
    let book = ContactBook::load(&home, &sub).unwrap();
    let mut names: Vec<&str> = book.contacts.iter().map(|c| c.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["Maciek", "Marek"]);
    // Same for the local provider.
    std::fs::create_dir_all(home.join("contacts")).unwrap();
    std::fs::write(home.join("contacts/x.json"), "[]").unwrap();
    overlay(&home, 3, "Ola", "never");
    let book = ContactBook::load(&home, &sub).unwrap();
    assert_eq!(book.contacts.len(), 3);
    assert_eq!(
        book.resolve("Ola").unwrap().policy.as_ref().unwrap().mode,
        Mode::Never
    );
}

#[test]
fn set_policy_writes_overlay() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (_root, sub) = fixture_repo(tmp.path());
    let mut book = ContactBook::load(&home, &sub).unwrap();
    let policy = Policy {
        mode: Mode::Auto,
        scope: Scope::default(),
        rate_limit_per_hour: Some(20),
    };
    book.set_policy(&home, &fp(1), policy.clone()).unwrap();
    assert_eq!(book.policy_for(&fp(1)), Some(&policy));
    let path = home.join("contacts").join(format!("{}.json", fp(1)));
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["added_at", "policy", "pubkey", "source"], "{v}");
    assert_eq!(v["pubkey"], pk(1));
    assert_eq!(v["source"], "local");
    assert_eq!(v["policy"]["mode"], "auto");
    assert_eq!(v["policy"]["rate_limit_per_hour"], 20);
    assert_eq!(v["policy"]["scope"]["projects"], serde_json::json!(["*"]));
    let added = v["added_at"].as_str().unwrap();
    assert_eq!(added.len(), 20, "{added}");
    assert!(added.starts_with("20") && added.ends_with('Z'), "{added}");
    // Reload: repo name kept, policy from overlay.
    let book = ContactBook::load(&home, &sub).unwrap();
    let c = book.resolve(&fp(1)).unwrap();
    assert_eq!(c.name, "Maciek");
    assert_eq!(c.policy.as_ref(), Some(&policy));
    // Update keeps the existing file's fields and swaps the policy.
    let mut book = book;
    let never = Policy {
        mode: Mode::Never,
        scope: Scope::default(),
        rate_limit_per_hour: None,
    };
    book.set_policy(&home, &fp(1), never.clone()).unwrap();
    let v2: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(v2["added_at"], added);
    assert_eq!(v2["policy"]["mode"], "never");
    assert!(v2["policy"].get("rate_limit_per_hour").is_none(), "{v2}");
    // Local-only contact: file gains policy, keeps name/emails/endpoints.
    overlay(&home, 3, "Ola", "manual");
    let mut book = ContactBook::load(&home, &sub).unwrap();
    book.set_policy(&home, &fp(3), never).unwrap();
    let v3: serde_json::Value = serde_json::from_slice(
        &std::fs::read(home.join("contacts").join(format!("{}.json", fp(3)))).unwrap(),
    )
    .unwrap();
    assert_eq!(v3["name"], "Ola");
    assert_eq!(v3["policy"]["mode"], "never");
    assert!(v3.get("fingerprint").is_none(), "{v3}");
    // Unknown fingerprint is an error and writes nothing.
    assert!(
        book.set_policy(
            &home,
            &fp(9),
            Policy {
                mode: Mode::Auto,
                scope: Scope::default(),
                rate_limit_per_hour: None
            }
        )
        .is_err()
    );
    assert!(
        !home
            .join("contacts")
            .join(format!("{}.json", fp(9)))
            .exists()
    );
}

fn auto() -> Policy {
    Policy {
        mode: Mode::Auto,
        scope: Scope::default(),
        rate_limit_per_hour: None,
    }
}

#[test]
fn set_policy_rejects_non_object_overlay_file() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (_root, sub) = fixture_repo(tmp.path());
    let dir = home.join("contacts");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{}.json", fp(1)));
    std::fs::write(&path, "[]").unwrap();
    let mut book = ContactBook::load(&home, &sub).unwrap();
    let err = book
        .set_policy(&home, &fp(1), auto())
        .unwrap_err()
        .to_string();
    assert!(err.contains("not a JSON object"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[]",
        "file untouched"
    );
    assert!(
        book.policy_for(&fp(1)).is_none(),
        "in-memory policy untouched"
    );
    // A string is not an object either.
    std::fs::write(&path, "\"x\"").unwrap();
    assert!(book.set_policy(&home, &fp(1), auto()).is_err());
}

#[test]
fn fingerprint_tier_beats_email_tier() {
    // Peer 3's email is literally peer 1's fingerprint string.
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (root, sub) = fixture_repo(tmp.path());
    std::fs::write(
        root.join(".agents/peers/aaa.json"),
        peer_json("Zed", &fp(1), 3),
    )
    .unwrap();
    let book = ContactBook::load(&home, &sub).unwrap();
    assert_eq!(book.contacts.len(), 3);
    assert_eq!(
        book.contacts[0].name, "Zed",
        "sorted first, so a naive scan would hit it"
    );
    let c = book.resolve(&fp(1)).unwrap();
    assert_eq!(c.name, "Maciek");
    assert_eq!(c.fingerprint(), fp(1));
}

#[test]
fn prefix_tier_matches_names_only() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (_root, sub) = fixture_repo(tmp.path());
    let book = ContactBook::load(&home, &sub).unwrap();
    // "maciek@" is a prefix of Maciek's email but of no name.
    assert!(book.resolve("maciek@").is_err());
    assert!(book.resolve("maciek@company").is_err());
    // Exact email still works.
    assert_eq!(book.resolve("maciek@company.com").unwrap().name, "Maciek");
}

#[test]
fn email_match_is_exact_case_sensitive() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (_root, sub) = fixture_repo(tmp.path());
    let book = ContactBook::load(&home, &sub).unwrap();
    assert_eq!(book.resolve("marek@company.com").unwrap().name, "Marek");
    assert!(book.resolve("Marek@company.com").is_err());
    assert!(book.resolve("MAREK@COMPANY.COM").is_err());
    assert!(book.resolve(" marek@company.com").is_err());
}

#[test]
fn git_file_marks_worktree_root() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (root, sub) = fixture_repo(tmp.path());
    std::fs::remove_dir(root.join(".git")).unwrap();
    std::fs::write(root.join(".git"), "gitdir: /elsewhere/.git/worktrees/x\n").unwrap();
    assert_eq!(
        owlpost::contacts::repo::find_git_root(&sub).as_deref(),
        Some(root.as_path())
    );
    let book = ContactBook::load(&home, &sub).unwrap();
    assert_eq!(book.contacts.len(), 2);
    assert!(book.contacts.iter().all(|c| c.source == "repo"));
}

#[test]
fn set_policy_on_slug_named_local_contact_writes_fp_file() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (_root, sub) = fixture_repo(tmp.path());
    let dir = home.join("contacts");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("ola.json"),
        serde_json::json!({
            "name": "Ola", "emails": ["ola@example.org"], "pubkey": pk(3),
            "endpoints": ["ola.example.org:7411"],
        })
        .to_string(),
    )
    .unwrap();
    let mut book = ContactBook::load(&home, &sub).unwrap();
    book.set_policy(&home, &fp(3), auto()).unwrap();
    let path = dir.join(format!("{}.json", fp(3)));
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "added_at",
            "emails",
            "endpoints",
            "name",
            "policy",
            "pubkey",
            "source"
        ],
        "{v}"
    );
    assert_eq!(v["name"], "Ola");
    assert_eq!(v["emails"], serde_json::json!(["ola@example.org"]));
    assert_eq!(v["endpoints"], serde_json::json!(["ola.example.org:7411"]));
    assert_eq!(v["pubkey"], pk(3));
    assert_eq!(v["source"], "local");
    assert_eq!(v["policy"]["mode"], "auto");
    assert!(v.get("fingerprint").is_none(), "{v}");
    assert!(dir.join("ola.json").exists(), "slug file left alone");
}

#[test]
fn non_json_extensions_are_ignored_silently() {
    // Loading in-process: stderr can't be captured here, so the CLI twin in tests/cli.rs
    // pins "no warning"; this one pins "not loaded".
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (root, sub) = fixture_repo(tmp.path());
    let peers = root.join(".agents/peers");
    std::fs::write(peers.join("upper.JSON"), peer_json("Upper", "u@x.org", 5)).unwrap();
    std::fs::write(peers.join("old.json.bak"), peer_json("Bak", "b@x.org", 6)).unwrap();
    std::fs::write(peers.join("noext"), peer_json("NoExt", "n@x.org", 7)).unwrap();
    let book = ContactBook::load(&home, &sub).unwrap();
    let mut names: Vec<&str> = book.contacts.iter().map(|c| c.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["Maciek", "Marek"]);
}

#[test]
fn contacts_are_ordered_by_filename() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let root = tmp.path().join("repo2");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let peers = root.join(".agents/peers");
    std::fs::create_dir_all(&peers).unwrap();
    // Filenames sort z < ... no: "a" < "b", but names sort the other way round.
    std::fs::write(peers.join("a.json"), peer_json("Zoe", "z@x.org", 1)).unwrap();
    std::fs::write(peers.join("b.json"), peer_json("Adam", "a@x.org", 2)).unwrap();
    let book = ContactBook::load(&home, &root).unwrap();
    let names: Vec<&str> = book.contacts.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Zoe", "Adam"]);
    // Same rule for the local provider.
    let dir = home.join("contacts");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("1.json"), peer_json("Yara", "y@x.org", 3)).unwrap();
    std::fs::write(dir.join("0.json"), peer_json("Bea", "b@x.org", 4)).unwrap();
    let book = ContactBook::load(&home, &root).unwrap();
    let names: Vec<&str> = book.contacts.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Zoe", "Adam", "Bea", "Yara"]);
}
