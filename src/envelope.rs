//! Signed payload envelope (§6): `Payload` JSON, `Envelope { raw, sig }`, question hash,
//! replay window (`is_fresh`, `SeenIds`).

use std::path::Path;

use anyhow::Context;
use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::identity::{self, Identity};

pub const REPLAY_WINDOW_SECS: u64 = 300;
pub const SEEN_IDS_TTL_SECS: u64 = 600;
const SEEN_IDS_FILE: &str = "seen-ids.txt";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Payload {
    pub v: u32,
    pub id: String,
    #[serde(rename = "type")]
    pub kind: Kind,
    pub from: String,
    pub to: String,
    pub ts: String,
    pub in_reply_to: Option<String>,
    pub body: Body,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Question,
    Answer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Body {
    Question {
        project: String,
        path: String,
        question: String,
    },
    Answer {
        answer: String,
        harness: String,
        redactions: u32,
        cached: bool,
    },
}

impl Payload {
    /// New question payload with a fresh UUIDv7 id and the current UTC timestamp.
    pub fn question(from: &str, to: &str, project: &str, path: &str, question: &str) -> Payload {
        Payload {
            v: 1,
            id: uuid::Uuid::now_v7().to_string(),
            kind: Kind::Question,
            from: from.into(),
            to: to.into(),
            ts: rfc3339_now(),
            in_reply_to: None,
            body: Body::Question {
                project: project.into(),
                path: path.into(),
                question: question.into(),
            },
        }
    }

    /// New answer payload replying to `question`.
    pub fn answer(
        question: &Payload,
        answer: &str,
        harness: &str,
        redactions: u32,
        cached: bool,
    ) -> Payload {
        Payload {
            v: 1,
            id: uuid::Uuid::now_v7().to_string(),
            kind: Kind::Answer,
            from: question.to.clone(),
            to: question.from.clone(),
            ts: rfc3339_now(),
            in_reply_to: Some(question.id.clone()),
            body: Body::Answer {
                answer: answer.into(),
                harness: harness.into(),
                redactions,
                cached,
            },
        }
    }

    /// Exact bytes that get signed: compact JSON in §6 field order.
    pub fn to_signed_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("Payload is always serialisable")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub raw: String,
    pub sig: String,
}

impl Envelope {
    pub fn sign(payload: &Payload, identity: &Identity) -> Envelope {
        let bytes = payload.to_signed_bytes();
        let sig = identity.sign(&bytes);
        Envelope {
            raw: String::from_utf8(bytes).expect("serde_json emits UTF-8"),
            sig: identity::sig_string(&sig),
        }
    }

    /// Verifies the signature over the raw bytes BEFORE parsing them.
    pub fn verify(&self, pubkey: &VerifyingKey) -> anyhow::Result<Payload> {
        let sig = identity::parse_sig(&self.sig)?;
        if !identity::verify(pubkey, self.raw.as_bytes(), &sig) {
            anyhow::bail!("signature does not verify");
        }
        serde_json::from_str(&self.raw).context("parsing payload")
    }
}

/// SHA-256 hex of `project\npath\nnorm(question)`; norm = trim, collapse whitespace, lowercase.
pub fn question_hash(project: &str, path: &str, question: &str) -> String {
    let norm = question
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let digest = Sha256::digest(format!("{project}\n{path}\n{norm}").as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// `|now − ts| ≤ window`. Unparsable `ts` is never fresh.
pub fn is_fresh(ts: &str, now_unix: u64, window_secs: u64) -> bool {
    match parse_rfc3339_to_unix(ts) {
        Some(t) => now_unix.abs_diff(t) <= window_secs,
        None => false,
    }
}

/// Recently seen payload ids with their unix timestamps; persisted one `id ts` per line.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct SeenIds {
    pub ids: Vec<(String, u64)>,
}

impl SeenIds {
    /// Missing file → empty. Entries older than `SEEN_IDS_TTL_SECS` relative to `now` are pruned.
    pub fn load(home: &Path, now_unix: u64) -> anyhow::Result<SeenIds> {
        let path = home.join(SEEN_IDS_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let ids = text
            .lines()
            .filter_map(|l| {
                let mut it = l.split_whitespace();
                Some((it.next()?.to_string(), it.next()?.parse().ok()?))
            })
            .filter(|(_, ts)| now_unix.saturating_sub(*ts) <= SEEN_IDS_TTL_SECS)
            .collect();
        Ok(SeenIds { ids })
    }

    /// Returns false (and does nothing) if `id` is already present.
    pub fn insert(&mut self, id: &str, now_unix: u64) -> bool {
        if self.ids.iter().any(|(i, _)| i == id) {
            return false;
        }
        self.ids.push((id.to_string(), now_unix));
        true
    }

    pub fn save(&self, home: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(home).with_context(|| format!("creating {}", home.display()))?;
        let path = home.join(SEEN_IDS_FILE);
        let text: String = self
            .ids
            .iter()
            .map(|(id, ts)| format!("{id} {ts}\n"))
            .collect();
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
    }
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `YYYY-MM-DDTHH:MM:SSZ` for the current time.
pub fn rfc3339_now() -> String {
    unix_to_rfc3339(now_unix())
}

// ponytail: UTC 'Z' only — add offsets if a peer ever sends them.
pub fn parse_rfc3339_to_unix(s: &str) -> Option<u64> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>());
    let mut t = time.split(':').map(|x| x.parse::<i64>());
    let (y, m, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let (h, mi, sec) = (t.next()?.ok()?, t.next()?.ok()?, t.next()?.ok()?);
    if d.next().is_some() || t.next().is_some() {
        return None;
    }
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let days = days_from_civil(y, m, day);
    u64::try_from(days * 86_400 + h * 3_600 + mi * 60 + sec).ok()
}

pub fn unix_to_rfc3339(unix: u64) -> String {
    let unix = unix as i64;
    let days = unix.div_euclid(86_400);
    let rem = unix.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

// Howard Hinnant's days_from_civil / civil_from_days (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0191c7a0-0000-7000-8000-000000000000";
    const TS: &str = "2026-09-01T10:00:00Z";
    const TS_UNIX: u64 = 1_788_256_800;

    fn fixed_question() -> Payload {
        Payload {
            v: 1,
            id: ID.into(),
            kind: Kind::Question,
            from: "owl:aaaaaaaaaaaaaaaa".into(),
            to: "owl:bbbbbbbbbbbbbbbb".into(),
            ts: TS.into(),
            in_reply_to: None,
            body: Body::Question {
                project: "github.com/company/monorepo".into(),
                path: "src/auth/session.rs".into(),
                question: "Why is the refresh token rotated on every read?".into(),
            },
        }
    }

    #[test]
    fn golden_question_json_matches_design_section_6() {
        let json = String::from_utf8(fixed_question().to_signed_bytes()).unwrap();
        assert_eq!(
            json,
            r#"{"v":1,"id":"0191c7a0-0000-7000-8000-000000000000","type":"question","from":"owl:aaaaaaaaaaaaaaaa","to":"owl:bbbbbbbbbbbbbbbb","ts":"2026-09-01T10:00:00Z","in_reply_to":null,"body":{"project":"github.com/company/monorepo","path":"src/auth/session.rs","question":"Why is the refresh token rotated on every read?"}}"#
        );
        let back: Payload = serde_json::from_str(&json).unwrap();
        assert_eq!(back, fixed_question());
    }

    #[test]
    fn golden_answer_json_matches_design_section_6() {
        let q = fixed_question();
        let mut a = Payload::answer(&q, "Because.", "claude", 2, true);
        a.id = ID.into();
        a.ts = TS.into();
        let json = String::from_utf8(a.to_signed_bytes()).unwrap();
        assert_eq!(
            json,
            r#"{"v":1,"id":"0191c7a0-0000-7000-8000-000000000000","type":"answer","from":"owl:bbbbbbbbbbbbbbbb","to":"owl:aaaaaaaaaaaaaaaa","ts":"2026-09-01T10:00:00Z","in_reply_to":"0191c7a0-0000-7000-8000-000000000000","body":{"answer":"Because.","harness":"claude","redactions":2,"cached":true}}"#
        );
        let back: Payload = serde_json::from_str(&json).unwrap();
        assert_eq!(back.kind, Kind::Answer);
        assert_eq!(back, a);
    }

    #[test]
    fn question_constructor_fills_uuid_v7_and_timestamp() {
        let p = Payload::question("owl:a", "owl:b", "proj", "p.rs", "why?");
        let u = uuid::Uuid::parse_str(&p.id).unwrap();
        assert_eq!(u.get_version_num(), 7);
        assert!(is_fresh(&p.ts, now_unix(), 5), "ts={}", p.ts);
        assert_eq!(p.v, 1);
        assert_eq!(p.kind, Kind::Question);
        assert_eq!(p.in_reply_to, None);
    }

    #[test]
    fn sign_then_verify() {
        let me = Identity::from_seed([1u8; 32]);
        let other = Identity::from_seed([2u8; 32]);
        let payload = fixed_question();
        let env = Envelope::sign(&payload, &me);
        assert_eq!(env.raw.as_bytes(), payload.to_signed_bytes().as_slice());
        assert!(env.sig.starts_with("ed25519:"));
        assert_eq!(env.verify(&me.verifying_key()).unwrap(), payload);
        assert!(
            env.verify(&other.verifying_key()).is_err(),
            "other key must reject"
        );
        let mut tampered = env.clone();
        tampered.raw.replace_range(2..3, "V"); // "v" -> "V": one character
        assert_ne!(tampered.raw, env.raw);
        assert!(
            tampered.verify(&me.verifying_key()).is_err(),
            "tampered raw must reject"
        );
        let mut bad_sig = env.clone();
        bad_sig.sig = "foo:AAAA".into();
        assert!(
            bad_sig.verify(&me.verifying_key()).is_err(),
            "bad sig prefix must reject"
        );
        // Valid signature over non-JSON bytes: verify passes, parse fails.
        let junk = Envelope {
            raw: "not json".into(),
            sig: identity::sig_string(&me.sign(b"not json")),
        };
        let err = junk.verify(&me.verifying_key()).err().unwrap().to_string();
        assert!(err.contains("parsing"), "{err}");
    }

    #[test]
    fn question_hash_normalises() {
        let a = question_hash("p", "f", "  Why  X? ");
        let b = question_hash("p", "f", "why x?");
        let c = question_hash("p", "f", "why y?");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 64);
        assert!(
            a.chars()
                .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
        );
        assert_ne!(
            question_hash("p2", "f", "why x?"),
            a,
            "project is part of the hash"
        );
        assert_ne!(
            question_hash("p", "f2", "why x?"),
            a,
            "path is part of the hash"
        );
        assert_eq!(
            question_hash("p", "f", "why\tx?\n"),
            a,
            "all whitespace collapses"
        );
        // Pinned: SHA-256("p\nf\nwhy x?")
        assert_eq!(
            a,
            "001767e04e6a17cd5059b7e8132c2b100099becb5f5e85cacef6cb69e73c5975"
        );
    }

    #[test]
    fn replay_window() {
        let now = TS_UNIX;
        assert!(!is_fresh(
            &unix_to_rfc3339(now - 301),
            now,
            REPLAY_WINDOW_SECS
        ));
        assert!(is_fresh(
            &unix_to_rfc3339(now - 299),
            now,
            REPLAY_WINDOW_SECS
        ));
        assert!(is_fresh(
            &unix_to_rfc3339(now - 300),
            now,
            REPLAY_WINDOW_SECS
        ));
        assert!(
            !is_fresh(&unix_to_rfc3339(now + 301), now, REPLAY_WINDOW_SECS),
            "future skew"
        );
        assert!(is_fresh(
            &unix_to_rfc3339(now + 299),
            now,
            REPLAY_WINDOW_SECS
        ));
        assert!(!is_fresh("garbage", now, REPLAY_WINDOW_SECS));
        assert!(
            !is_fresh("2026-09-01T10:00:00", now, REPLAY_WINDOW_SECS),
            "missing Z"
        );

        let home = tempfile::tempdir().unwrap();
        let mut seen = SeenIds::load(home.path(), now).unwrap();
        assert!(seen.ids.is_empty());
        assert!(seen.insert(ID, now));
        assert!(!seen.insert(ID, now), "second insert must be false");
        assert!(
            seen.insert("other", now - 601),
            "old entry inserted for pruning check"
        );
        seen.save(home.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(home.path().join("seen-ids.txt")).unwrap(),
            format!("{ID} {now}\nother {}\n", now - 601)
        );
        let mut loaded = SeenIds::load(home.path(), now).unwrap();
        assert!(!loaded.insert(ID, now), "must survive save/load");
        assert!(
            loaded.insert("other", now),
            "entries older than 600 s are pruned on load"
        );
        assert_eq!(loaded.ids.len(), 2);
    }

    #[test]
    fn rfc3339_roundtrip_and_rejects() {
        assert_eq!(parse_rfc3339_to_unix(TS), Some(TS_UNIX));
        assert_eq!(unix_to_rfc3339(TS_UNIX), TS);
        assert_eq!(parse_rfc3339_to_unix("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(unix_to_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(
            parse_rfc3339_to_unix("2000-03-01T00:00:00Z"),
            Some(951_868_800)
        );
        assert_eq!(unix_to_rfc3339(951_868_799), "2000-02-29T23:59:59Z");
        for bad in [
            "",
            "2026-09-01T10:00:00",
            "2026-09-01 10:00:00Z",
            "2026-13-01T10:00:00Z",
            "2026-09-01T24:00:00Z",
            "2026-09-01T10:00:00+02:00",
            "2026-09-01T10:00:00.000Z",
            "1969-12-31T23:59:59Z",
        ] {
            assert_eq!(parse_rfc3339_to_unix(bad), None, "{bad:?}");
        }
        assert_eq!(rfc3339_now().len(), 20);
        assert!(rfc3339_now().ends_with('Z'));
    }
}
