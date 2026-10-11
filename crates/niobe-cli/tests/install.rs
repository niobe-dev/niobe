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
    release_holding(root, "niobe", stand_in)
}

/// A release whose archives hold the script `stand_in` as the file `name`.
fn release_holding(root: &Path, name: &str, stand_in: &str) -> PathBuf {
    let staging = root.join("staging");
    fs::create_dir_all(&staging).expect("the temporary directory is writable");
    let binary = staging.join(name);
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
            .arg(name)
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
    script(&tools.join("wget"), &gnu_wget(&used));
    // `shasum -a 256 <file>`, answered by whichever SHA-256 tool this machine
    // really has.
    let digest = match (system_tool(&["sha256sum"]), system_tool(&["shasum"])) {
        (Some(sum), _) => format!("'{}' \"$3\"", sum.display()),
        (None, Some(sum)) => format!("'{}' -a 256 \"$3\"", sum.display()),
        (None, None) => {
            // A check this machine cannot make, which a CI runner must be
            // able to: skipped quietly there, it would prove nothing.
            assert!(
                std::env::var_os("CI").is_none(),
                "this machine has neither sha256sum nor shasum to stand in for"
            );
            return;
        }
    };
    script(
        &tools.join("shasum"),
        &format!("echo shasum >> '{}'\n{digest}\n", used.display()),
    );

    let output = install_with_tools(root.path(), &release, &tools);

    assert!(output.status.success(), "{}", said(&output));
    let used = fs::read_to_string(&used).expect("the stand-ins were run");
    assert!(used.contains("wget"), "{used}");
    assert!(used.contains("shasum"), "{used}");
    assert_installed(root.path());
}

/// Runs the installer as [`install`] does, with `tools` as the whole PATH.
fn install_with_tools(root: &Path, release: &Path, tools: &Path) -> Output {
    Command::new("sh")
        .arg(installer())
        .env_clear()
        .env("PATH", tools)
        .env("HOME", root)
        .env("NIOBE_INSTALL_DIR", root.join("bin"))
        .env(
            "NIOBE_DOWNLOAD_URL",
            format!("file://{}", release.display()),
        )
        .output()
        .expect("sh is on every machine the tests run on")
}

/// That `root/bin/niobe` is the stand-in the release holds, and runs.
fn assert_installed(root: &Path) {
    let version = Command::new(root.join("bin/niobe"))
        .arg("--version")
        .output()
        .expect("the installer made the binary executable");
    assert_eq!(String::from_utf8_lossy(&version.stdout), "niobe 9.9.9\n");
}

/// The body of a stand-in `wget` that reads `<url> -O <file>` among its
/// arguments the way the real one does, fetching the `file://` release by
/// copying it, and notes in `used` that it ran. `--help` is answered, and
/// anything else that opens with `-` is passed over.
fn wget_reading(used: &Path, help: &str, long_options: &str) -> String {
    format!(
        "case \"$1\" in --help) echo '{help}' >&2; exit 1 ;; esac
for arg in \"$@\"; do
    case \"$arg\" in --*) {long_options} ;; esac
done
echo wget >> '{used}'
url= file=
while [ $# -gt 0 ]; do
    case \"$1\" in
        -O) file=$2; shift ;;
        -*) ;;
        *) url=$1 ;;
    esac
    shift
done
cp \"${{url#file://}}\" \"$file\"
",
        used = used.display()
    )
}

/// GNU wget, which restricts itself to HTTPS when asked.
fn gnu_wget(used: &Path) -> String {
    wget_reading(used, "  --https-only   only follow secure HTTPS links", ":")
}

/// BusyBox wget, which Alpine ships in place of curl: it has no
/// `--https-only` and refuses every long option it does not know.
fn busybox_wget(used: &Path) -> String {
    wget_reading(
        used,
        "Usage: wget [-cqS] [--spider] [-O FILE] [-o LOGFILE] [--header STR] URL...",
        "case \"$arg\" in --spider|--header) ;; *) echo \"wget: unrecognized option: ${arg#--}\" >&2; exit 1 ;; esac",
    )
}

/// Alpine has BusyBox's `wget` and no `curl`; the installer downloads with
/// the options that `wget` has rather than failing on one it lacks.
#[test]
fn the_installer_downloads_with_a_wget_that_has_no_https_only_option() {
    let root = tempfile::tempdir().expect("a temporary directory can be made");
    let release = release(root.path());
    let tools = root.path().join("tools");
    tools_without(&tools, &["curl", "wget"]);
    let used = root.path().join("used");
    script(&tools.join("wget"), &busybox_wget(&used));

    let output = install_with_tools(root.path(), &release, &tools);

    assert!(output.status.success(), "{}", said(&output));
    let used = fs::read_to_string(&used).expect("the stand-in was run");
    assert!(used.contains("wget"), "{used}");
    assert_installed(root.path());
}

/// A download that fails says which tool failed it, since the URL is rarely
/// what is wrong.
#[test]
fn a_failed_download_names_the_tool_that_failed() {
    let root = tempfile::tempdir().expect("a temporary directory can be made");
    let empty = root.path().join("empty");
    fs::create_dir_all(&empty).expect("the temporary directory is writable");
    let tools = root.path().join("tools");
    tools_without(&tools, &["curl", "wget"]);
    script(
        &tools.join("wget"),
        &busybox_wget(&root.path().join("used")),
    );

    let output = install_with_tools(root.path(), &empty, &tools);

    assert!(!output.status.success(), "{}", said(&output));
    assert!(
        said(&output).contains("wget could not download"),
        "{}",
        said(&output)
    );
    assert!(!root.path().join("bin/niobe").exists());
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
    fs::write(&elsewhere, "someone else's").expect("the temporary directory is writable");
    fs::create_dir_all(root.path().join("bin")).expect("the directory is made");
    std::os::unix::fs::symlink(&elsewhere, root.path().join("bin").join("niobe"))
        .expect("the link is made");

    let output = install(root.path(), &release);

    assert!(!output.status.success(), "{}", said(&output));
    assert!(said(&output).contains("is a link"), "{}", said(&output));
    assert_nothing_staged(root.path());
    assert_eq!(
        fs::read_to_string(&elsewhere)
            .expect("the file is in a temporary directory and was just written"),
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

/// Runs the installer with `tools` as the whole PATH and exactly the
/// environment `env` names.
fn run_installer(tools: &Path, env: &[(&str, &str)]) -> Output {
    let mut command = Command::new("sh");
    command.arg(installer()).env_clear().env("PATH", tools);
    for (name, value) in env {
        command.env(name, value);
    }
    command
        .output()
        .expect("sh is on every machine the tests run on")
}

/// A stand-in `curl` that writes its arguments on one line of `used`, and
/// fetches a `file://` URL by copying it: any other URL is a failed download,
/// whose message then names the URL the installer built.
fn recording_curl(used: &Path) -> String {
    format!(
        "echo \"$*\" >> '{used}'
url= file=
while [ $# -gt 0 ]; do
    case \"$1\" in
        -o) file=$2; shift ;;
        --proto) shift ;;
        -*) ;;
        *) url=$1 ;;
    esac
    shift
done
case \"$url\" in
    file://*) cp \"${{url#file://}}\" \"$file\" ;;
    *) exit 1 ;;
esac
",
        used = used.display()
    )
}

/// The arguments each stand-in was run with, one line to a run.
fn argv_of(used: &Path) -> String {
    fs::read_to_string(used).expect("the stand-in was run")
}

#[test]
fn curl_is_held_to_https_and_a_current_tls() {
    let root = tempfile::tempdir().expect("a temporary directory can be made");
    let release = release(root.path());
    let tools = root.path().join("tools");
    tools_without(&tools, &["curl", "wget"]);
    let used = root.path().join("used");
    script(&tools.join("curl"), &recording_curl(&used));

    let output = install_with_tools(root.path(), &release, &tools);

    assert!(output.status.success(), "{}", said(&output));
    for line in argv_of(&used).lines() {
        assert!(line.contains("--proto =https,file"), "{line}");
        assert!(line.contains("--tlsv1.2"), "{line}");
    }
}

#[test]
fn a_wget_that_can_be_held_to_https_is() {
    let root = tempfile::tempdir().expect("a temporary directory can be made");
    let release = release(root.path());
    let tools = root.path().join("tools");
    tools_without(&tools, &["curl", "wget"]);
    let used = root.path().join("used");
    script(
        &tools.join("wget"),
        &format!(
            "echo \"$*\" >> '{}.argv'\n{}",
            used.display(),
            gnu_wget(&used)
        ),
    );

    let output = install_with_tools(root.path(), &release, &tools);

    assert!(output.status.success(), "{}", said(&output));
    let argv =
        fs::read_to_string(format!("{}.argv", used.display())).expect("the stand-in was run");
    let downloads: Vec<&str> = argv.lines().filter(|line| line.contains("-O")).collect();
    assert!(!downloads.is_empty(), "{argv}");
    for line in downloads {
        assert!(line.contains("--https-only"), "{line}");
    }
}

#[test]
fn a_wget_that_cannot_be_held_to_https_refuses_a_plain_http_url() {
    let root = tempfile::tempdir().expect("a temporary directory can be made");
    let tools = root.path().join("tools");
    tools_without(&tools, &["curl", "wget"]);
    let used = root.path().join("used");
    script(&tools.join("wget"), &busybox_wget(&used));

    let output = run_installer(
        &tools,
        &[
            ("HOME", &root.path().display().to_string()),
            (
                "NIOBE_INSTALL_DIR",
                &root.path().join("bin").display().to_string(),
            ),
            ("NIOBE_DOWNLOAD_URL", "http://releases.example/niobe"),
        ],
    );

    assert!(!output.status.success(), "{}", said(&output));
    assert!(
        said(&output).contains("is not an https:// URL"),
        "{}",
        said(&output)
    );
    assert!(!used.exists(), "a plain-http URL was fetched");
}

/// A machine as `os` and `arch`, which `uname` is made to report, with the
/// Rosetta answer `rosetta` for Apple silicon, and a curl that fails every
/// download and so names the URL it was asked for.
fn on_machine(
    root: &Path,
    os: &str,
    arch: &str,
    rosetta: Option<&str>,
    env: &[(&str, &str)],
) -> Output {
    let tools = root.join("tools");
    tools_without(&tools, &["curl", "wget", "uname", "sysctl"]);
    script(
        &tools.join("uname"),
        &format!("case \"$1\" in -s) echo '{os}' ;; -m) echo '{arch}' ;; esac\n"),
    );
    if let Some(answer) = rosetta {
        script(&tools.join("sysctl"), &format!("echo {answer}\n"));
    }
    script(&tools.join("curl"), &recording_curl(&root.join("used")));
    let home = root.display().to_string();
    let mut all = vec![("HOME", home.as_str())];
    all.extend_from_slice(env);
    run_installer(&tools, &all)
}

#[test]
fn an_operating_system_with_no_release_is_named() {
    let root = tempfile::tempdir().expect("a temporary directory can be made");

    let output = on_machine(root.path(), "Plan9", "x86_64", None, &[]);

    assert!(!output.status.success(), "{}", said(&output));
    assert!(
        said(&output).contains("niobe-install: error: no release is built for Plan9"),
        "{}",
        said(&output)
    );
}

#[test]
fn an_architecture_with_no_release_is_named() {
    let root = tempfile::tempdir().expect("a temporary directory can be made");

    let output = on_machine(root.path(), "Linux", "riscv64", None, &[]);

    assert!(!output.status.success(), "{}", said(&output));
    assert!(
        said(&output).contains("no release is built for riscv64"),
        "{}",
        said(&output)
    );
}

#[test]
fn a_shell_under_rosetta_installs_the_native_binary() {
    let root = tempfile::tempdir().expect("a temporary directory can be made");

    let output = on_machine(root.path(), "Darwin", "x86_64", Some("1"), &[]);

    assert!(
        said(&output).contains("niobe-aarch64-apple-darwin.tar.gz"),
        "{}",
        said(&output)
    );
}

#[test]
fn an_intel_mac_installs_the_intel_binary() {
    let root = tempfile::tempdir().expect("a temporary directory can be made");

    let output = on_machine(root.path(), "Darwin", "x86_64", Some("0"), &[]);

    assert!(
        said(&output).contains("niobe-x86_64-apple-darwin.tar.gz"),
        "{}",
        said(&output)
    );
}

#[test]
fn each_form_of_the_requested_version_names_its_release_url() {
    let base = "https://github.com/niobe-dev/niobe/releases";
    for (version, url) in [
        (None, format!("{base}/latest/download/")),
        (Some("v1.2.3"), format!("{base}/download/v1.2.3/")),
        (Some("1.2.3"), format!("{base}/download/v1.2.3/")),
    ] {
        let root = tempfile::tempdir().expect("a temporary directory can be made");
        let env: Vec<(&str, &str)> = version.map(|v| ("NIOBE_VERSION", v)).into_iter().collect();

        let output = on_machine(root.path(), "Linux", "x86_64", None, &env);

        assert!(
            said(&output).contains(&format!("{url}niobe-x86_64-unknown-linux-musl.tar.gz")),
            "{version:?}: {}",
            said(&output)
        );
    }
}

#[test]
fn a_machine_with_no_downloader_says_so() {
    let root = tempfile::tempdir().expect("a temporary directory can be made");
    let tools = root.path().join("tools");
    tools_without(&tools, &["curl", "wget"]);

    let output = install_with_tools(root.path(), &root.path().join("release"), &tools);

    assert!(!output.status.success(), "{}", said(&output));
    assert!(
        said(&output).contains("needs curl or wget"),
        "{}",
        said(&output)
    );
}

#[test]
fn a_machine_with_no_sha_tool_says_so_and_installs_nothing() {
    let root = tempfile::tempdir().expect("a temporary directory can be made");
    let release = release(root.path());
    let tools = root.path().join("tools");
    tools_without(&tools, &["sha256sum", "shasum"]);

    let output = install_with_tools(root.path(), &release, &tools);

    assert!(!output.status.success(), "{}", said(&output));
    assert!(
        said(&output).contains("needs sha256sum or shasum"),
        "{}",
        said(&output)
    );
    assert!(!root.path().join("bin/niobe").exists());
}

#[test]
fn an_archive_that_holds_no_niobe_installs_nothing() {
    let root = tempfile::tempdir().expect("a temporary directory can be made");
    let release = release_holding(root.path(), "something-else", STAND_IN);

    let output = install(root.path(), &release);

    assert!(!output.status.success(), "{}", said(&output));
    assert!(
        said(&output).contains("holds no niobe binary"),
        "{}",
        said(&output)
    );
    assert!(!root.path().join("bin/niobe").exists());
}

#[test]
fn a_machine_without_claude_is_told_niobe_drives_it() {
    let root = tempfile::tempdir().expect("a temporary directory can be made");
    let release = release(root.path());

    let output = install(root.path(), &release);

    assert!(output.status.success(), "{}", said(&output));
    assert!(
        said(&output).contains("'claude' CLI, which is not on PATH"),
        "{}",
        said(&output)
    );
}

#[test]
fn a_machine_with_claude_is_not_told_so() {
    let root = tempfile::tempdir().expect("a temporary directory can be made");
    let release = release(root.path());
    let tools = root.path().join("tools");
    tools_without(&tools, &[]);
    script(&tools.join("claude"), "exit 0\n");

    let output = install_with_tools(root.path(), &release, &tools);

    assert!(output.status.success(), "{}", said(&output));
    assert!(!said(&output).contains("'claude'"), "{}", said(&output));
}

#[test]
fn an_empty_home_with_no_install_directory_ends_in_the_installers_own_message() {
    let root = tempfile::tempdir().expect("a temporary directory can be made");
    let release = release(root.path());
    let url = format!("file://{}", release.display());
    let tools = root.path().join("tools");
    tools_without(&tools, &[]);

    for env in [
        vec![("HOME", ""), ("NIOBE_DOWNLOAD_URL", url.as_str())],
        vec![("NIOBE_DOWNLOAD_URL", url.as_str())],
    ] {
        let output = run_installer(&tools, &env);

        assert!(!output.status.success(), "{}", said(&output));
        assert!(
            said(&output).contains("niobe-install: error: HOME is not set"),
            "{}",
            said(&output)
        );
    }
}

#[test]
fn a_directory_at_the_staging_name_is_refused_and_left_alone() {
    let root = tempfile::tempdir().expect("a temporary directory can be made");
    let release = release(root.path());
    let in_the_way = root.path().join("bin").join(".niobe.new");
    fs::create_dir_all(&in_the_way).expect("the directory is made");

    let output = install(root.path(), &release);

    assert!(!output.status.success(), "{}", said(&output));
    assert!(
        said(&output).contains("niobe-install: error:") && said(&output).contains("in the way"),
        "{}",
        said(&output)
    );
    assert!(
        fs::read_dir(&in_the_way)
            .expect("the directory is still there")
            .next()
            .is_none(),
        "the binary was copied into the directory"
    );
    assert!(!root.path().join("bin/niobe").exists());
}
