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
    let staging = root.join("staging");
    fs::create_dir_all(&staging).expect("the temporary directory is writable");
    let binary = staging.join("niobe");
    fs::write(&binary, STAND_IN).expect("the staging directory is writable");
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
