//! OWL-015 AC2: `scripts/install.sh` installs a working `owl` from a release layout and fails
//! closed on every guard. Nothing here touches the network: a tarball of the test binary plus
//! a `SHA256SUMS` are written to a temp dir and served through `OWL_INSTALL_BASE_URL=file://`,
//! which `curl` reads like any other URL. Each guard has its own row with the message unique
//! to it (a shared epilogue would let `contains` match every exit path, see `_lessons.md`).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const INSTALL_SH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/install.sh");
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Same OS/arch mapping the script uses, for the host this test runs on.
fn host_target() -> String {
    let os = match std::env::consts::OS {
        "linux" => "unknown-linux-gnu",
        "macos" => "apple-darwin",
        other => panic!("tests run on linux or macos only, got {other}"),
    };
    format!("{}-{}", std::env::consts::ARCH, os)
}

/// A release directory holding `owl-<VERSION>-<host target>.tar.gz` and `SHA256SUMS`, built
/// from the binary under test.
struct Release {
    dir: tempfile::TempDir,
    asset: String,
}

impl Release {
    fn build() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let asset = format!("owl-{VERSION}-{}.tar.gz", host_target());
        let bin_dir = Path::new(env!("CARGO_BIN_EXE_owl")).parent().unwrap();
        let tar = Command::new("tar")
            .args(["-C"])
            .arg(bin_dir)
            .arg("-czf")
            .arg(dir.path().join(&asset))
            .arg("owl")
            .output()
            .expect("tar is required to build the fixture tarball");
        assert!(tar.status.success(), "{}", stderr(&tar));
        let sum = sha256(&dir.path().join(&asset));
        fs::write(dir.path().join("SHA256SUMS"), format!("{sum}  {asset}\n")).unwrap();
        Self { dir, asset }
    }

    fn base_url(&self) -> String {
        format!("file://{}", self.dir.path().display())
    }
}

fn sha256(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let bytes = fs::read(path).unwrap();
    format!("{:x}", Sha256::digest(bytes))
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// Runs `sh scripts/install.sh <args>` with the hook variables cleared, then `env` applied.
fn install(args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new("sh");
    c.arg(INSTALL_SH).args(args).stdin(Stdio::null());
    for k in [
        "OWL_INSTALL_BASE_URL",
        "OWL_INSTALL_FAKE_SUM",
        "OWL_INSTALL_OS",
        "OWL_INSTALL_ARCH",
        "OWL_INSTALL_CURL",
    ] {
        c.env_remove(k);
    }
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn prefix() -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("bin");
    (d, p)
}

fn owl_version(prefix: &Path) -> String {
    let out = Command::new(prefix.join("owl"))
        .arg("--version")
        .env_remove("OWLPOST_HOME")
        .output()
        .expect("installed owl runs");
    assert!(out.status.success(), "{}", stderr(&out));
    stdout(&out).trim().to_string()
}

#[test]
fn script_is_executable_posix_sh() {
    let meta = fs::metadata(INSTALL_SH).expect("scripts/install.sh exists");
    assert!(meta.is_file());
    assert_ne!(meta.permissions().mode() & 0o111, 0, "must be executable");
    let src = fs::read_to_string(INSTALL_SH).unwrap();
    assert!(
        src.starts_with("#!/bin/sh\n"),
        "shebang: {:?}",
        src.lines().next()
    );
    assert!(
        src.contains("\nset -eu\n"),
        "must fail on errors and unset vars"
    );
    // Every hook the tests rely on is documented in the header.
    for hook in [
        "OWL_INSTALL_BASE_URL",
        "OWL_INSTALL_FAKE_SUM",
        "OWL_INSTALL_OS",
        "OWL_INSTALL_ARCH",
        "OWL_INSTALL_CURL",
    ] {
        let header: String = src.lines().take_while(|l| l.starts_with('#')).collect();
        assert!(header.contains(hook), "header must document {hook}");
    }
    // The real download base is the GitHub releases URL of this repository.
    assert!(src.contains("https://github.com/${REPO}/releases/download/${tag}"));
    assert!(src.contains("REPO=\"Krab00/owlpost\""));
}

// ---------------------------------------------------------------- AC2: install works

#[test]
fn installs_the_tagged_version_into_prefix() {
    let rel = Release::build();
    let (_d, pfx) = prefix();
    let tag = format!("v{VERSION}");
    let out = install(
        &["--version", &tag, "--prefix", pfx.to_str().unwrap()],
        &[("OWL_INSTALL_BASE_URL", &rel.base_url())],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stderr(&out).is_empty(), "{}", stderr(&out));

    // The binary is there, executable, and reports the tag's version.
    let bin = pfx.join("owl");
    assert_ne!(
        fs::metadata(&bin).unwrap().permissions().mode() & 0o111,
        0,
        "installed owl must be executable"
    );
    assert_eq!(owl_version(&pfx), format!("owl {VERSION}"));
    // No temp file left next to it.
    let names: Vec<_> = fs::read_dir(&pfx)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, vec!["owl".to_string()]);

    // Stdout: what was fetched, where it landed, the three next steps in order.
    let so = stdout(&out);
    assert!(
        so.contains(&format!("downloading {}/{}", rel.base_url(), rel.asset)),
        "{so}"
    );
    assert!(
        so.contains(&format!("installed owl {VERSION} to {}/owl", pfx.display())),
        "{so}"
    );
    let steps: Vec<&str> = so
        .lines()
        .filter_map(|l| l.strip_prefix("  owl "))
        .map(|l| l.split('#').next().unwrap().trim())
        .collect();
    assert_eq!(steps, ["init", "install", "contact export"]);
    assert!(so.contains("next steps:"), "{so}");
}

#[test]
fn version_without_v_prefix_and_bare_tag_both_install() {
    let rel = Release::build();
    let (_d, pfx) = prefix();
    let out = install(
        &["--version", VERSION, "--prefix", pfx.to_str().unwrap()],
        &[("OWL_INSTALL_BASE_URL", &rel.base_url())],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(owl_version(&pfx), format!("owl {VERSION}"));
}

#[test]
fn without_version_reads_the_asset_name_from_sha256sums() {
    let rel = Release::build();
    let (_d, pfx) = prefix();
    let out = install(
        &["--prefix", pfx.to_str().unwrap()],
        &[("OWL_INSTALL_BASE_URL", &rel.base_url())],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stdout(&out).contains(&format!("installed owl {VERSION} to")),
        "{}",
        stdout(&out)
    );
    assert_eq!(owl_version(&pfx), format!("owl {VERSION}"));
}

#[test]
fn reinstall_overwrites_an_existing_binary() {
    let rel = Release::build();
    let (_d, pfx) = prefix();
    fs::create_dir_all(&pfx).unwrap();
    fs::write(pfx.join("owl"), "#!/bin/sh\necho stale\n").unwrap();
    let out = install(
        &["--version", VERSION, "--prefix", pfx.to_str().unwrap()],
        &[("OWL_INSTALL_BASE_URL", &rel.base_url())],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(owl_version(&pfx), format!("owl {VERSION}"));
}

#[test]
fn prefix_on_path_gets_no_path_note() {
    let rel = Release::build();
    let (_d, pfx) = prefix();
    fs::create_dir_all(&pfx).unwrap();
    let path = format!(
        "{}:{}",
        pfx.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = install(
        &["--version", VERSION, "--prefix", pfx.to_str().unwrap()],
        &[("OWL_INSTALL_BASE_URL", &rel.base_url()), ("PATH", &path)],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        !stdout(&out).contains("is not on your PATH"),
        "{}",
        stdout(&out)
    );
    // And the twin: a prefix outside PATH gets the note.
    let (_d2, pfx2) = prefix();
    let out = install(
        &["--version", VERSION, "--prefix", pfx2.to_str().unwrap()],
        &[("OWL_INSTALL_BASE_URL", &rel.base_url())],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stdout(&out).contains(&format!(
            "note: {} is not on your PATH; add it to your shell profile",
            pfx2.display()
        )),
        "{}",
        stdout(&out)
    );
}

// ---------------------------------------------------------------- AC2: checksum mismatch

#[test]
fn fake_sum_fails_with_checksum_mismatch_and_installs_nothing() {
    let rel = Release::build();
    let (_d, pfx) = prefix();
    let out = install(
        &["--version", VERSION, "--prefix", pfx.to_str().unwrap()],
        &[
            ("OWL_INSTALL_BASE_URL", &rel.base_url()),
            ("OWL_INSTALL_FAKE_SUM", "1"),
        ],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    let real = sha256(&rel.dir.path().join(&rel.asset));
    assert_eq!(
        err.trim(),
        format!(
            "owl install: checksum mismatch for {}: expected {}, got {real}",
            rel.asset,
            "0".repeat(64)
        )
    );
    assert!(!pfx.join("owl").exists(), "nothing may be installed");
    let leftovers: Vec<_> = fs::read_dir(&pfx).unwrap().collect();
    assert!(
        leftovers.is_empty(),
        "prefix must stay empty: {leftovers:?}"
    );
}

#[test]
fn a_tampered_tarball_fails_the_real_checksum() {
    // Not the hook: the SHA256SUMS is genuine and the tarball is modified after signing.
    let rel = Release::build();
    let path = rel.dir.path().join(&rel.asset);
    let mut bytes = fs::read(&path).unwrap();
    bytes.push(0);
    fs::write(&path, &bytes).unwrap();
    let (_d, pfx) = prefix();
    let out = install(
        &["--version", VERSION, "--prefix", pfx.to_str().unwrap()],
        &[("OWL_INSTALL_BASE_URL", &rel.base_url())],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).starts_with(&format!(
            "owl install: checksum mismatch for {}: expected ",
            rel.asset
        )),
        "{}",
        stderr(&out)
    );
    assert!(!pfx.join("owl").exists());
}

#[test]
fn fake_sum_unset_or_not_one_does_not_corrupt() {
    let rel = Release::build();
    let (_d, pfx) = prefix();
    let out = install(
        &["--version", VERSION, "--prefix", pfx.to_str().unwrap()],
        &[
            ("OWL_INSTALL_BASE_URL", &rel.base_url()),
            ("OWL_INSTALL_FAKE_SUM", "0"),
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(owl_version(&pfx), format!("owl {VERSION}"));
}

// ---------------------------------------------------------------- guards, one row each

#[test]
fn unsupported_platform_fails_before_anything_is_fetched() {
    let (_d, pfx) = prefix();
    for (os, arch, shown) in [
        ("Linux", "riscv64", "Linux/riscv64"),
        ("FreeBSD", "x86_64", "FreeBSD/x86_64"),
        ("Darwin", "i386", "Darwin/i386"),
    ] {
        let out = install(
            &["--prefix", pfx.to_str().unwrap()],
            &[
                ("OWL_INSTALL_OS", os),
                ("OWL_INSTALL_ARCH", arch),
                ("OWL_INSTALL_BASE_URL", "file:///nonexistent"),
            ],
        );
        assert_eq!(out.status.code(), Some(2), "{shown}: {}", stderr(&out));
        assert_eq!(
            stderr(&out).trim(),
            format!(
                "owl install: unsupported platform {shown} (supported: macOS and Linux on x86_64 or aarch64)"
            )
        );
        assert!(stdout(&out).is_empty());
    }
    assert!(
        !pfx.exists(),
        "prefix is created only for a supported platform"
    );
}

#[test]
fn every_supported_pair_maps_to_its_triple() {
    // The positive twin of the platform guard: each pair resolves to the asset name for its
    // triple (observed through the "not listed" message, so no build per target is needed).
    let rel = Release::build();
    let (_d, pfx) = prefix();
    for (os, arch, triple) in [
        ("Darwin", "arm64", "aarch64-apple-darwin"),
        ("Darwin", "aarch64", "aarch64-apple-darwin"),
        ("Darwin", "x86_64", "x86_64-apple-darwin"),
        ("Linux", "x86_64", "x86_64-unknown-linux-gnu"),
        ("Linux", "amd64", "x86_64-unknown-linux-gnu"),
        ("Linux", "aarch64", "aarch64-unknown-linux-gnu"),
        ("Linux", "arm64", "aarch64-unknown-linux-gnu"),
    ] {
        let out = install(
            &["--version", "v9.9.9", "--prefix", pfx.to_str().unwrap()],
            &[
                ("OWL_INSTALL_OS", os),
                ("OWL_INSTALL_ARCH", arch),
                ("OWL_INSTALL_BASE_URL", &rel.base_url()),
            ],
        );
        assert_eq!(out.status.code(), Some(1), "{os}/{arch}: {}", stderr(&out));
        assert_eq!(
            stderr(&out).trim(),
            format!("owl install: owl-9.9.9-{triple}.tar.gz is not listed in SHA256SUMS"),
            "{os}/{arch}"
        );
    }
}

#[test]
fn missing_curl_fails_with_its_own_message() {
    let (_d, pfx) = prefix();
    let out = install(
        &["--prefix", pfx.to_str().unwrap()],
        &[("OWL_INSTALL_CURL", "owl-test-no-such-curl")],
    );
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert_eq!(
        stderr(&out).trim(),
        "owl install: curl is required but was not found on PATH"
    );
    assert!(stdout(&out).is_empty());
}

#[test]
fn download_failure_names_the_url() {
    let (_d, pfx) = prefix();
    let missing = tempfile::tempdir().unwrap();
    let base = format!("file://{}/no-such-release", missing.path().display());
    let out = install(
        &["--version", "v1.2.3", "--prefix", pfx.to_str().unwrap()],
        &[("OWL_INSTALL_BASE_URL", &base)],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(
        stderr(&out).trim(),
        format!("owl install: download failed for {base}/SHA256SUMS")
    );
    assert!(!pfx.join("owl").exists());

    // Second arm: SHA256SUMS lists the asset but the tarball itself is gone.
    let rel = Release::build();
    fs::remove_file(rel.dir.path().join(&rel.asset)).unwrap();
    let out = install(
        &["--version", VERSION, "--prefix", pfx.to_str().unwrap()],
        &[("OWL_INSTALL_BASE_URL", &rel.base_url())],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(
        stderr(&out).trim(),
        format!(
            "owl install: download failed for {}/{}",
            rel.base_url(),
            rel.asset
        )
    );
}

#[test]
fn version_not_listed_in_sha256sums_fails() {
    let rel = Release::build();
    let (_d, pfx) = prefix();
    let out = install(
        &["--version", "v9.9.9", "--prefix", pfx.to_str().unwrap()],
        &[("OWL_INSTALL_BASE_URL", &rel.base_url())],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(
        stderr(&out).trim(),
        format!(
            "owl install: owl-9.9.9-{}.tar.gz is not listed in SHA256SUMS",
            host_target()
        )
    );
}

#[test]
fn latest_without_an_asset_for_this_target_fails() {
    let rel = Release::build();
    fs::write(
        rel.dir.path().join("SHA256SUMS"),
        format!(
            "{}  owl-{VERSION}-riscv64gc-unknown-linux-gnu.tar.gz\n",
            "a".repeat(64)
        ),
    )
    .unwrap();
    let (_d, pfx) = prefix();
    let out = install(
        &["--prefix", pfx.to_str().unwrap()],
        &[("OWL_INSTALL_BASE_URL", &rel.base_url())],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(
        stderr(&out).trim(),
        format!(
            "owl install: no asset for {} listed in {}/SHA256SUMS",
            host_target(),
            rel.base_url()
        )
    );
}

#[test]
fn unwritable_prefix_fails_before_download() {
    let d = tempfile::tempdir().unwrap();
    let ro = d.path().join("ro");
    fs::create_dir(&ro).unwrap();
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(ro.join("probe"), b"").is_ok() {
        // Running as root: the read-only bit does not bind, the guard cannot be induced.
        eprintln!("skipping: prefix is writable despite 0555 (root?)");
        return;
    }
    let out = install(
        &["--version", VERSION, "--prefix", ro.to_str().unwrap()],
        &[("OWL_INSTALL_BASE_URL", "file:///nonexistent")],
    );
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(
        stderr(&out).trim(),
        format!(
            "owl install: prefix {} is not writable (use --prefix <dir> you own, or sudo for --system)",
            ro.display()
        )
    );
    assert!(stdout(&out).is_empty(), "nothing was downloaded");

    // Sibling arm: the prefix cannot be created because its parent is a file.
    let file = d.path().join("file");
    fs::write(&file, b"").unwrap();
    let under_file = file.join("bin");
    let out = install(
        &[
            "--version",
            VERSION,
            "--prefix",
            under_file.to_str().unwrap(),
        ],
        &[("OWL_INSTALL_BASE_URL", "file:///nonexistent")],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).starts_with(&format!(
            "owl install: prefix {} is not writable",
            under_file.display()
        )),
        "{}",
        stderr(&out)
    );
}

#[test]
fn usage_errors_are_distinct_and_exit_2() {
    let cases: [(&[&str], &str); 4] = [
        (
            &["--version"],
            "owl install: --version needs a value (e.g. --version v0.1.0)",
        ),
        (
            &["--prefix"],
            "owl install: --prefix needs a value (e.g. --prefix ~/.local/bin)",
        ),
        (
            &["--bogus"],
            "owl install: unknown argument '--bogus' (see --help)",
        ),
        (
            &["--version", "v1", "extra"],
            "owl install: unknown argument 'extra' (see --help)",
        ),
    ];
    for (args, msg) in cases {
        let out = install(args, &[]);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
        assert_eq!(stderr(&out).trim(), msg, "{args:?}");
        assert!(stdout(&out).is_empty(), "{args:?}");
    }
}

#[test]
fn help_prints_flags_and_exits_0() {
    for flag in ["--help", "-h"] {
        let out = install(&[flag], &[]);
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
        let so = stdout(&out);
        for token in [
            "--version <tag>",
            "--prefix <dir>",
            "--system",
            "curl -fsSL",
        ] {
            assert!(so.contains(token), "{flag}: missing {token} in {so}");
        }
        assert!(stderr(&out).is_empty());
    }
}

#[test]
fn system_flag_targets_usr_local_bin() {
    // /usr/local/bin is either writable (then the fetch from a dead base fails) or not (then
    // the prefix guard names it); either way the flag resolved to that path.
    let out = install(
        &["--system", "--version", "v1.2.3"],
        &[("OWL_INSTALL_BASE_URL", "file:///nonexistent-owl-base")],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(
        err.trim()
            == "owl install: prefix /usr/local/bin is not writable (use --prefix <dir> you own, or sudo for --system)"
            || err.trim()
                == "owl install: download failed for file:///nonexistent-owl-base/SHA256SUMS",
        "{err}"
    );
}
