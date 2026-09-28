// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Runs `install.sh`, the script users install niobe with, against a release
//! laid out in a temporary directory the way the release workflow publishes
//! one: `niobe-<target>.tar.gz` and `niobe-<target>.tar.gz.sha256` for every
//! target, fetched through `NIOBE_DOWNLOAD_URL` as `file://` URLs.
//!
//! The binary in each archive is a shell script standing in for niobe, so the
//! test proves what the installer does with an archive, not what the release
//! build puts in one.

#![cfg(unix)]
#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sha2::{Digest, Sha256};

/// Every target the release workflow builds, named the way `install.sh`
/// names this machine.
const TARGETS: &[&str] = &[
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-musl",
    "x86_64-unknown-linux-musl",
];

const STAND_IN: &str = "#!/bin/sh\necho 'niobe 9.9.9'\n";

fn installer() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../install.sh")
}

/// A release directory with an archive and a checksum for every target.
fn release(root: &Path) -> PathBuf {
    release_of(root, STAND_IN)
}

/// A release whose binary is the script `stand_in`.
fn release_of(root: &Path, stand_in: &str) -> PathBuf {
    let staging = root.join("staging");
    fs::create_dir_all(&staging).expect("the temporary directory is writable");
    let binary = staging.join("niobe");
    fs::write(&binary, stand_in).expect("the staging directory is writable");
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))
        .expect("the stand-in was just written");

    let release = root.join("release");
    fs::create_dir_all(&release).expect("the temporary directory is writable");
    for target in TARGETS {
        let archive = release.join(format!("niobe-{target}.tar.gz"));
        let status = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&staging)
            .arg("niobe")
            .status()
            .expect("tar is on PATH on every machine the tests run on");
        assert!(
            status.success(),
            "tar could not build {}",
            archive.display()
        );
        let bytes = fs::read(&archive).expect("tar just wrote the archive");
        let line = format!("{}  niobe-{target}.tar.gz\n", hex(&Sha256::digest(&bytes)));
        fs::write(release.join(format!("niobe-{target}.tar.gz.sha256")), line)
            .expect("the release directory is writable");
    }
    release
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Runs the installer from `release` into `root/bin`, with a PATH that has the
/// system's tools on it and not the install directory.
fn install(root: &Path, release: &Path) -> Output {
    Command::new("sh")
        .arg(installer())
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", root)
        .env("NIOBE_INSTALL_DIR", root.join("bin"))
        .env(
            "NIOBE_DOWNLOAD_URL",
            format!("file://{}", release.display()),
        )
        .output()
        .expect("sh is on every machine the tests run on")
}

fn said(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn the_installer_puts_a_working_niobe_in_the_install_directory() {
    let root = tempfile::tempdir().expect("a temporary directory can be made");
    let release = release(root.path());

    let output = install(root.path(), &release);

    assert!(output.status.success(), "{}", said(&output));
    let installed = root.path().join("bin/niobe");
    let version = Command::new(&installed)
        .arg("--version")
        .output()
        .expect("the installer made the binary executable");
    assert_eq!(String::from_utf8_lossy(&version.stdout), "niobe 9.9.9\n");
    assert!(
        said(&output).contains(&format!("installed niobe 9.9.9 to {}", installed.display())),
        "{}",
        said(&output)
    );
}

#[test]
fn an_install_directory_that_is_not_on_path_is_named_with_the_line_that_adds_it() {
    let root = tempfile::tempdir().expect("a temporary directory can be made");
    let release = release(root.path());

    let output = install(root.path(), &release);

    let bin = root.path().join("bin");
    assert!(
        said(&output).contains(&format!("{} is not on your PATH", bin.display())),
        "{}",
        said(&output)
    );
    assert!(
        said(&output).contains(&format!("export PATH=\"{}:$PATH\"", bin.display())),
        "{}",
        said(&output)
    );
}

#[test]
fn an_archive_that_does_not_match_its_checksum_installs_nothing() {
    let root = tempfile::tempdir().expect("a temporary directory can be made");
    let release = release(root.path());
    for target in TARGETS {
        fs::write(
            release.join(format!("niobe-{target}.tar.gz.sha256")),
            format!("{}  niobe-{target}.tar.gz\n", "0".repeat(64)),
        )
        .expect("the release directory is writable");
    }

    let output = install(root.path(), &release);

    assert!(!output.status.success(), "{}", said(&output));
    assert!(
        said(&output).contains("does not match its published SHA-256"),
        "{}",
        said(&output)
    );
    assert!(!root.path().join("bin/niobe").exists());
}

#[test]
fn a_release_that_cannot_be_downloaded_says_which_file_and_installs_nothing() {
    let root = tempfile::tempdir().expect("a temporary directory can be made");
    let empty = root.path().join("empty");
    fs::create_dir_all(&empty).expect("the temporary directory is writable");

    let output = install(root.path(), &empty);

    assert!(!output.status.success(), "{}", said(&output));
    assert!(
        said(&output).contains("could not download"),
        "{}",
        said(&output)
    );
    assert!(!root.path().join("bin/niobe").exists());
}

/// Every tool in the system's directories, linked into `dir`, but for the
/// ones `leave_out` names: a machine with fewer tools than this one.
fn tools_without(dir: &Path, leave_out: &[&str]) {
    fs::create_dir_all(dir).expect("the temporary directory is writable");
    for system in ["/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
        let Ok(entries) = fs::read_dir(system) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let link = dir.join(&name);
            if leave_out.iter().any(|left| name == *left) || link.exists() {
                continue;
            }
            let _ = std::os::unix::fs::symlink(entry.path(), link);
        }
    }
}

/// Writes an executable `sh` script at `path`.
fn script(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}")).expect("the directory is writable");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .expect("the script was just written");
}

/// The first of `names` in the system's directories.
fn system_tool(names: &[&str]) -> Option<PathBuf> {
    names.iter().find_map(|name| {
        ["/usr/bin", "/bin", "/usr/sbin", "/sbin"]
            .iter()
            .map(|dir| Path::new(dir).join(name))
            .find(|path| path.is_file())
    })
}

/// A machine with neither `curl` nor `sha256sum` downloads with `wget` and
/// verifies with `shasum`, and installs what it verified.
#[test]
fn the_installer_falls_back_to_wget_and_shasum() {
    let root = tempfile::tempdir().expect("a temporary directory can be made");
    let release = release(root.path());
    let tools = root.path().join("tools");
    tools_without(&tools, &["curl", "wget", "sha256sum", "shasum"]);
    let used = root.path().join("used");
    // `wget -q --https-only <url> -O <file>`, reading the `file://` release
    // the way the real one reads a URL.
    script(
        &tools.join("wget"),
        &format!(
            "echo wget >> '{used}'\nurl=$3\ncp \"${{url#file://}}\" \"$5\"\n",
            used = used.display()
        ),
    );
    // `shasum -a 256 <file>`, answered by whichever SHA-256 tool this machine
    // really has.
    let digest = match (system_tool(&["sha256sum"]), system_tool(&["shasum"])) {
        (Some(sum), _) => format!("'{}' \"$3\"", sum.display()),
        (None, Some(sum)) => format!("'{}' -a 256 \"$3\"", sum.display()),
        (None, None) => return,
    };
    script(
        &tools.join("shasum"),
        &format!("echo shasum >> '{}'\n{digest}\n", used.display()),
    );

    let output = Command::new("sh")
        .arg(installer())
        .env_clear()
        .env("PATH", &tools)
        .env("HOME", root.path())
        .env("NIOBE_INSTALL_DIR", root.path().join("bin"))
        .env(
            "NIOBE_DOWNLOAD_URL",
            format!("file://{}", release.display()),
        )
        .output()
        .expect("sh is on every machine the tests run on");

    assert!(output.status.success(), "{}", said(&output));
    let used = fs::read_to_string(&used).expect("the stand-ins were run");
    assert!(used.contains("wget"), "{used}");
    assert!(used.contains("shasum"), "{used}");
    let version = Command::new(root.path().join("bin/niobe"))
        .arg("--version")
        .output()
        .expect("the installer made the binary executable");
    assert_eq!(String::from_utf8_lossy(&version.stdout), "niobe 9.9.9\n");
}

/// What the install directory holds after a failed install: the staged copy
/// must not be among it.
fn assert_nothing_staged(root: &Path) {
    assert!(
        !root.join("bin").join(".niobe.new").exists(),
        "the staged copy was left behind"
    );
}

#[test]
fn an_install_target_that_is_a_directory_fails_and_says_so() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let release = release(root.path());
    fs::create_dir_all(root.path().join("bin").join("niobe")).expect("the directory is made");

    let output = install(root.path(), &release);

    assert!(!output.status.success(), "{}", said(&output));
    assert!(
        said(&output).contains("is a directory"),
        "{}",
        said(&output)
    );
    assert!(
        !said(&output).contains("installed niobe"),
        "{}",
        said(&output)
    );
    assert_nothing_staged(root.path());
    assert!(
        fs::read_dir(root.path().join("bin").join("niobe"))
            .expect("the directory is still there")
            .next()
            .is_none(),
        "the binary was moved into the directory"
    );
}

#[test]
fn an_install_target_that_is_a_link_is_left_alone() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let release = release(root.path());
    let elsewhere = root.path().join("elsewhere");
    fs::write(&elsewhere, "someone else's").expect("written");
    fs::create_dir_all(root.path().join("bin")).expect("the directory is made");
    std::os::unix::fs::symlink(&elsewhere, root.path().join("bin").join("niobe"))
        .expect("the link is made");

    let output = install(root.path(), &release);

    assert!(!output.status.success(), "{}", said(&output));
    assert!(said(&output).contains("is a link"), "{}", said(&output));
    assert_nothing_staged(root.path());
    assert_eq!(
        fs::read_to_string(&elsewhere).expect("read"),
        "someone else's"
    );
}

#[test]
fn a_binary_that_does_not_run_installs_nothing() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let release = release_of(
        root.path(),
        "#!/bin/sh\necho 'exec format error' >&2\nexit 126\n",
    );

    let output = install(root.path(), &release);

    assert!(!output.status.success(), "{}", said(&output));
    assert!(said(&output).contains("does not run"), "{}", said(&output));
    assert!(
        said(&output).contains("exec format error"),
        "{}",
        said(&output)
    );
    assert_nothing_staged(root.path());
    assert!(!root.path().join("bin").join("niobe").exists());
}
