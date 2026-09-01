use owlpost::spool::{Dir, Record, Spool};

fn rec(state: &str) -> Record {
    Record {
        raw: r#"{"v":1}"#.into(),
        sig: "ed25519:AAAA".into(),
        state: state.into(),
        seen: false,
        received_at: "2026-09-01T10:00:00Z".into(),
        draft: None,
        meta: serde_json::Value::Null,
    }
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[test]
fn new_creates_five_dirs() {
    let home = tempfile::tempdir().unwrap();
    Spool::new(home.path()).unwrap();
    for d in ["inbox", "outbox", "asks", "done", "cache"] {
        assert!(home.path().join("spool").join(d).is_dir(), "{d}");
    }
    // Idempotent.
    Spool::new(home.path()).unwrap();
}

#[test]
fn atomic_put_leaves_no_temp_files() {
    let home = tempfile::tempdir().unwrap();
    let spool = Spool::new(home.path()).unwrap();
    for i in 0..100 {
        spool
            .put(Dir::Inbox, &format!("id-{i:03}"), &rec("pending"))
            .unwrap();
    }
    let files = walk(&home.path().join("spool"));
    let tmp: Vec<_> = files
        .iter()
        .filter(|p| p.to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(tmp.is_empty(), "leftover temp files: {tmp:?}");
    assert_eq!(files.len(), 100);
    let listed = spool.list(Dir::Inbox, |_| true).unwrap();
    assert_eq!(listed.len(), 100);
    assert_eq!(listed[0].0, "id-000");
    assert_eq!(listed[99].0, "id-099");
    assert!(listed.iter().all(|(_, r)| r.state == "pending"));
    // Overwrite is atomic too and does not duplicate.
    spool.put(Dir::Inbox, "id-000", &rec("drafted")).unwrap();
    assert_eq!(spool.list(Dir::Inbox, |_| true).unwrap().len(), 100);
    assert_eq!(
        spool.get(Dir::Inbox, "id-000").unwrap().unwrap().state,
        "drafted"
    );
    assert!(
        !walk(&home.path().join("spool"))
            .iter()
            .any(|p| p.to_string_lossy().ends_with(".tmp"))
    );
}

#[test]
fn stray_temp_files_are_not_listed() {
    let home = tempfile::tempdir().unwrap();
    let spool = Spool::new(home.path()).unwrap();
    spool.put(Dir::Inbox, "a", &rec("pending")).unwrap();
    std::fs::write(home.path().join("spool/inbox/b.json.tmp"), "{garbage").unwrap();
    std::fs::write(home.path().join("spool/inbox/notes.txt"), "x").unwrap();
    let listed = spool.list(Dir::Inbox, |_| true).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0, "a");
}

#[test]
fn state_transitions() {
    let home = tempfile::tempdir().unwrap();
    let spool = Spool::new(home.path()).unwrap();
    spool.put(Dir::Inbox, "q1", &rec("consent")).unwrap();
    assert_eq!(spool.count_unseen(Dir::Inbox).unwrap(), 1);
    spool.mark_seen(Dir::Inbox, "q1").unwrap();
    assert_eq!(spool.count_unseen(Dir::Inbox).unwrap(), 0);
    assert!(spool.get(Dir::Inbox, "q1").unwrap().unwrap().seen);

    spool.set_state(Dir::Inbox, "q1", "pending").unwrap();
    assert_eq!(
        spool.get(Dir::Inbox, "q1").unwrap().unwrap().state,
        "pending"
    );
    spool.set_state(Dir::Inbox, "q1", "drafted").unwrap();
    let r = spool.get(Dir::Inbox, "q1").unwrap().unwrap();
    assert_eq!(r.state, "drafted");
    assert!(r.seen, "set_state must keep seen");
    assert_eq!(r.raw, r#"{"v":1}"#, "set_state must keep raw");
    spool.set_state(Dir::Inbox, "q1", "done").unwrap();
    spool.move_to(Dir::Inbox, "q1", Dir::Done).unwrap();
    assert_eq!(spool.get(Dir::Done, "q1").unwrap().unwrap().state, "done");
    assert!(spool.get(Dir::Inbox, "q1").unwrap().is_none());
    assert_eq!(spool.count_unseen(Dir::Inbox).unwrap(), 0);
    assert_eq!(spool.list(Dir::Inbox, |_| true).unwrap().len(), 0);

    // Unseen count only counts unseen, and list filter works.
    spool.put(Dir::Inbox, "q2", &rec("consent")).unwrap();
    spool.put(Dir::Inbox, "q3", &rec("pending")).unwrap();
    spool.mark_seen(Dir::Inbox, "q3").unwrap();
    assert_eq!(spool.count_unseen(Dir::Inbox).unwrap(), 1);
    let pending = spool.list(Dir::Inbox, |r| r.state == "pending").unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].0, "q3");

    assert!(spool.set_state(Dir::Inbox, "missing", "x").is_err());
    assert!(spool.mark_seen(Dir::Inbox, "missing").is_err());
}

#[test]
fn move_to_removes_source() {
    let home = tempfile::tempdir().unwrap();
    let spool = Spool::new(home.path()).unwrap();
    spool.put(Dir::Asks, "a1", &rec("waiting")).unwrap();
    spool.move_to(Dir::Asks, "a1", Dir::Done).unwrap();
    assert!(!home.path().join("spool/asks/a1.json").exists());
    assert!(home.path().join("spool/done/a1.json").exists());
    assert_eq!(
        spool.get(Dir::Done, "a1").unwrap().unwrap().state,
        "waiting"
    );
    assert!(
        spool.move_to(Dir::Asks, "a1", Dir::Done).is_err(),
        "second move fails"
    );
}

#[test]
fn get_missing_is_none() {
    let home = tempfile::tempdir().unwrap();
    let spool = Spool::new(home.path()).unwrap();
    for d in Dir::ALL {
        assert!(spool.get(d, "nope").unwrap().is_none(), "{}", d.name());
    }
    std::fs::write(home.path().join("spool/inbox/bad.json"), "{broken").unwrap();
    assert!(
        spool.get(Dir::Inbox, "bad").is_err(),
        "malformed is an error, not None"
    );
}

#[test]
fn record_roundtrip_keeps_draft_and_meta() {
    let home = tempfile::tempdir().unwrap();
    let spool = Spool::new(home.path()).unwrap();
    let mut r = rec("drafted");
    r.draft = Some(serde_json::json!({"answer": "x", "harness": "fake"}));
    r.meta = serde_json::json!({"peer": "owl:abc"});
    r.seen = true;
    spool.put(Dir::Inbox, "d", &r).unwrap();
    assert_eq!(spool.get(Dir::Inbox, "d").unwrap().unwrap(), r);
    // §6 stored shape: field names present in the file.
    let text = std::fs::read_to_string(home.path().join("spool/inbox/d.json")).unwrap();
    for key in [
        "\"raw\"",
        "\"sig\"",
        "\"state\"",
        "\"seen\"",
        "\"received_at\"",
        "\"draft\"",
        "\"meta\"",
    ] {
        assert!(text.contains(key), "{key} missing in {text}");
    }
    // Older files without seen/draft/meta still load.
    std::fs::write(
        home.path().join("spool/inbox/old.json"),
        r#"{"raw":"{}","sig":"ed25519:AA","state":"pending","received_at":"2026-01-01T00:00:00Z"}"#,
    )
    .unwrap();
    let old = spool.get(Dir::Inbox, "old").unwrap().unwrap();
    assert!(!old.seen);
    assert_eq!(old.draft, None);
    assert_eq!(old.meta, serde_json::Value::Null);
}

#[test]
fn cache_roundtrip() {
    let home = tempfile::tempdir().unwrap();
    let spool = Spool::new(home.path()).unwrap();
    let hash = owlpost::envelope::question_hash("proj", "path", "why?");
    let mut r = rec("cached");
    r.raw = r#"{"type":"answer"}"#.into();
    assert!(spool.cache_get(&hash).unwrap().is_none());
    spool.cache_put(&hash, &r).unwrap();
    assert_eq!(spool.cache_get(&hash).unwrap(), Some(r.clone()));
    assert!(
        home.path()
            .join("spool/cache")
            .join(format!("{hash}.json"))
            .exists()
    );
    let other = owlpost::envelope::question_hash("proj", "path", "why not?");
    assert_ne!(other, hash);
    assert!(spool.cache_get(&other).unwrap().is_none());
    assert!(
        spool.get(Dir::Inbox, &hash).unwrap().is_none(),
        "cache lives only in cache/"
    );
}
