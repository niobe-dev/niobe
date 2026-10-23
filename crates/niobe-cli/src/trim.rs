// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! `trim = true`: the `claude` CLI hands a cargo command's output to the agent
//! through `niobe hook trim`, which leaves cargo's progress and the tests that
//! passed out of it ([`niobe_core::trim`]) and keeps the whole of it on disk.
//!
//! The hook is the CLI's `PostToolUse` hook on `Bash`. The CLI hands it the
//! call and what the command printed; what the hook prints back, as the
//! call's `updatedToolOutput`, is what the agent reads in its place. It has
//! to be the shape the CLI's own report has, `stdout` and `stderr` and the
//! rest: a bare string in its place is passed over, and the agent reads the
//! whole output. The report the hook writes also names the file the whole
//! output went to, `niobeRawOutputPath`, and its size, which the CLI keeps
//! in its stream and its transcript beside the rest of the report.
//!
//! The CLI takes one `--settings`: given two, it reads the last. A profile
//! that names a settings file of its own is started with a copy of that file
//! with the hook added to its `hooks`, so the operator's settings and the
//! hook both hold.
//!
//! What the hook writes, and the copy of the profile's settings, are in
//! [`DIR`] under the repository's `.niobe/`, which ignores all of it for git:
//! the outputs hold whatever the commands printed, and the settings whatever
//! the operator put in theirs.
//! A whole output is kept for [`KEEP_DAYS`] days: each one the hook writes
//! clears out those older than that.
//!
//! The hook changes nothing it cannot account for. A call that is not a
//! shell command, a command `rtk` already ran (RTK, a hook of its own, filters
//! cargo's output before this one sees it), an output the CLI already saved
//! to a file, an interrupted one, or one [`niobe_core::trim::trim`] would not
//! shorten, is handed back as it was — and so is every call when anything
//! goes wrong, since a hook that failed would fail the call.
//!
//! A command that fails never reaches it. The CLI runs `PostToolUse` only
//! after a call succeeds; after one that fails it runs `PostToolUseFailure`,
//! which takes context to add and no output to replace, so a failing run is
//! read whole, as the CLI hands it over.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The directory under `.niobe/` the whole outputs and the settings copy are
/// written to.
pub const DIR: &str = "trim";

/// The file in [`DIR`] that ignores everything beside it for git, itself
/// included.
const IGNORE: &str = "# Written by niobe: the whole outputs of trimmed commands, and the\n\
# settings the claude CLI is started with. Not meant to be committed.\n*\n";

/// What the copy of a profile's settings, with the hook added, is named in
/// [`DIR`], before the profile's name.
const SETTINGS: &str = "claude-settings-";

/// The most of its input the hook reads: the CLI hands it the command's whole
/// output, and one past this is handed back untrimmed rather than held.
const INPUT_LIMIT: u64 = 64 * 1024 * 1024;

/// How many days a whole output is kept after the hook wrote it. It is read
/// back while the session that ran the command is open, and from the CLI's
/// own transcript when that session is imported; a store's session holds the
/// whole output in its own record. Past this, an import reads the call as
/// the agent read it, trimmed.
const KEEP_DAYS: u64 = 14;

/// The CLI's shell tool, the one call whose output is trimmed.
const SHELL_TOOL: &str = "Bash";

/// Where a session's trimmed outputs are kept, and the binary the hook runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trim {
    /// This binary, which the CLI runs as the hook.
    pub exe: PathBuf,
    /// The repository's `.niobe/` [`DIR`].
    pub dir: PathBuf,
}

/// The settings the `claude` CLI is started with under `trim`: the own file
/// at `settings` of the profile named `profile`, where it names one, with the
/// hook added to it, written into [`DIR`]; the path of that copy is what is
/// returned.
///
/// Each profile has a copy of its own. The CLI reads its settings again when
/// the file changes, so a second session under another profile writing the
/// same file would hand the first that profile's environment.
///
/// [`DIR`] is made, with its ignore file, where it is not there yet. An error
/// says which file could not be read or written, and why.
pub fn settings(trim: &Trim, profile: &str, settings: Option<&str>) -> Result<String, String> {
    let mut document = match settings {
        Some(path) => {
            let text = niobe_config::read::text(Path::new(path)).map_err(|e| {
                format!("cannot read the settings file {path} to add the trim hook to it: {e}")
            })?;
            serde_json::from_str(&text).map_err(|e| {
                format!("cannot add the trim hook to the settings file {path}: it is not JSON: {e}")
            })?
        }
        None => json!({}),
    };
    add_hook(&mut document, &hook_command(trim)).map_err(|problem| {
        format!(
            "cannot add the trim hook to the settings file {}: {problem}",
            settings.unwrap_or_default()
        )
    })?;
    prepare(&trim.dir).map_err(|e| format!("cannot make {}: {e}", trim.dir.display()))?;
    let path = trim.dir.join(settings_name(profile));
    let text = serde_json::to_string_pretty(&document)
        .map_err(|e| format!("cannot write the settings with the trim hook added: {e}"))?;
    write_private(&path, text.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path.display().to_string())
}

/// The name of `profile`'s copy of its settings: the profile's name, with
/// every byte that is not a letter, a digit or `-` written as `_` and its
/// two hex digits, so that no name leaves [`DIR`] and no two names meet.
fn settings_name(profile: &str) -> String {
    let mut name = SETTINGS.to_owned();
    for byte in profile.bytes() {
        match byte.is_ascii_alphanumeric() || byte == b'-' {
            true => name.push(char::from(byte)),
            false => name.push_str(&format!("_{byte:02x}")),
        }
    }
    name.push_str(".json");
    name
}

/// The command the CLI runs as the hook: this binary, told where to keep the
/// whole outputs. Both paths are quoted for the `sh` the CLI runs it with.
fn hook_command(trim: &Trim) -> String {
    format!(
        "{} hook trim --raw {}",
        quoted(&trim.exe.display().to_string()),
        quoted(&trim.dir.display().to_string())
    )
}

/// `text` as one word to `sh`, whatever is in it.
fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// Adds the hook to the `PostToolUse` hooks of the settings `document`,
/// after any it has. Refused where the document's `hooks` are not shaped as
/// the CLI reads them, since what to keep of them would be a guess.
fn add_hook(document: &mut Value, command: &str) -> Result<(), &'static str> {
    let root = document.as_object_mut().ok_or("it is not a JSON object")?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("its `hooks` is not an object")?;
    let post = hooks
        .entry("PostToolUse")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or("its `hooks.PostToolUse` is not a list")?;
    post.push(json!({
        "matcher": SHELL_TOOL,
        "hooks": [{ "type": "command", "command": command }],
    }));
    Ok(())
}

/// Makes `dir`, readable by the operator alone, and its ignore file, where
/// they are not there yet. A `dir` that is a link is refused: it would choose
/// where the outputs go.
fn prepare(dir: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "it is there and is not a directory",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => create_private_dir(dir)?,
        Err(error) => return Err(error),
    }
    let ignore = dir.join(".gitignore");
    match std::fs::symlink_metadata(&ignore) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            write_new(&ignore, IGNORE.as_bytes())
        }
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// Writes `bytes` to a new file at `path`, readable by the operator alone.
/// A file, or a link, already at `path` is an error rather than written
/// through.
fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.flush()
}

/// Replaces the file at `path` with `bytes`: written beside it under a name
/// of its own, then renamed over it, so a link planted at `path` is replaced
/// rather than written through, and a reader never sees half of it.
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut staged = path.as_os_str().to_owned();
    staged.push(format!(".{}", std::process::id()));
    let staged = PathBuf::from(staged);
    let _stale = std::fs::remove_file(&staged);
    write_new(&staged, bytes)?;
    std::fs::rename(&staged, path)
}

/// The hook: what to print in answer to the call the CLI wrote to `input`,
/// keeping the whole output in `dir`. `None` hands the call back as it was,
/// which is what printing nothing does.
pub fn hook(input: impl Read, dir: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    input
        .take(INPUT_LIMIT.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > INPUT_LIMIT {
        return None;
    }
    let call: Value = serde_json::from_slice(&bytes).ok()?;
    if call.get("tool_name")?.as_str()? != SHELL_TOOL {
        return None;
    }
    let command = call.get("tool_input")?.get("command")?.as_str()?;
    if ran_by_rtk(command) {
        return None;
    }
    let report = call.get("tool_response")?.as_object()?;
    let flag = |key: &str| report.get(key).and_then(Value::as_bool).unwrap_or(false);
    if flag("interrupted") || flag("isImage") || report.contains_key("persistedOutputPath") {
        return None;
    }
    let text = |key: &str| report.get(key).and_then(Value::as_str).unwrap_or_default();
    let whole = joined(text("stdout"), text("stderr"));
    let trimmed = niobe_core::trim::trim(command, &whole)?;
    let id = call.get("tool_use_id")?.as_str()?;
    let path = dir.join(format!("{}.txt", file_name(id)?));
    prepare(dir).ok()?;
    write_new(&path, whole.as_bytes()).ok()?;
    prune(dir, std::time::SystemTime::now());

    let mut updated = report.clone();
    let note = note(trimmed.left_out(), whole.len(), trimmed.text().len(), &path);
    updated.insert(
        "stdout".to_owned(),
        json!(format!("{}{note}", trimmed.text())),
    );
    updated.insert("stderr".to_owned(), json!(""));
    updated.insert(
        "niobeRawOutputPath".to_owned(),
        json!(path.display().to_string()),
    );
    updated.insert("niobeRawOutputSize".to_owned(), json!(whole.len()));
    let answer = json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "updatedToolOutput": updated,
        }
    });
    Some(answer.to_string())
}

/// Removes the whole outputs in `dir` written more than [`KEEP_DAYS`] before
/// `now`, so that a repository the hook runs in every day does not keep
/// every command's output for ever. Only a file the hook would have written
/// is removed — a regular file named as [`file_name`] names one, `.txt` —
/// never a link, the settings copies or anything else put there. A file
/// that cannot be read or removed is left as it is: tidying up never fails
/// the call.
fn prune(dir: &Path, now: std::time::SystemTime) {
    let keep = std::time::Duration::from_secs(KEEP_DAYS.saturating_mul(86_400));
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let ours = path.extension().is_some_and(|ext| ext == "txt")
            && path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(file_name)
                .is_some();
        // `DirEntry::metadata` does not follow a link, so a link is seen as one.
        let expired = entry
            .metadata()
            .ok()
            .filter(|meta| meta.is_file())
            .and_then(|meta| {
                let written = meta.modified().ok()?;
                now.duration_since(written).ok()
            });
        if ours && expired.is_some_and(|age| age > keep) {
            let _left = std::fs::remove_file(&path);
        }
    }
}

/// Whether `command` is one RTK rewrote to run through itself, whose output
/// RTK has filtered already. Trimming what a filter left would leave out
/// lines on a reading of a format the filter changed.
fn ran_by_rtk(command: &str) -> bool {
    let first = command.split_whitespace().next().unwrap_or_default();
    first.rsplit('/').next() == Some("rtk")
}

/// What the agent reads for a command that printed `stdout` and `stderr`:
/// the CLI hands it both, a line apart, and either alone where the other is
/// empty.
fn joined(stdout: &str, stderr: &str) -> String {
    match (stdout.is_empty(), stderr.is_empty()) {
        (_, true) => stdout.to_owned(),
        (true, false) => stderr.to_owned(),
        (false, false) => format!("{stdout}\n{stderr}"),
    }
}

/// The name of the file a call's whole output goes to: its id, where that is
/// a name that stays in the directory — letters, digits, `_` and `-` only.
fn file_name(id: &str) -> Option<&str> {
    let fits = !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    fits.then_some(id)
}

/// What the agent reads under a trimmed output: what was left out and where
/// the whole of it is, so it can read that when the lines left out matter.
fn note(left_out: usize, whole: usize, kept: usize, path: &Path) -> String {
    let separator = match left_out {
        1 => "line",
        _ => "lines",
    };
    format!(
        "\n[niobe left out {left_out} {separator} of cargo's build progress and passing tests, \
         {whole} bytes down to {kept}. The whole output is in {}]\n",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> tempfile::TempDir {
        tempfile::tempdir().expect("a scratch directory is made")
    }

    fn passing_run() -> String {
        let tests: String = (0..60)
            .map(|n| format!("test tests::case_number_{n}_holds ... ok\n"))
            .collect();
        format!(
            "   Compiling demo v0.1.0 (/work/demo)\n\
             \x20   Finished `test` profile [unoptimized + debuginfo] target(s) in 0.66s\n\
             \x20    Running unittests src/lib.rs (target/debug/deps/demo-1)\n\
             \n\
             running 60 tests\n\
             {tests}\
             \n\
             test result: ok. 60 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\
             \n"
        )
    }

    fn call(command: &str, stdout: &str, stderr: &str) -> String {
        json!({
            "session_id": "s",
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash",
            "tool_input": { "command": command },
            "tool_response": {
                "stdout": stdout, "stderr": stderr, "interrupted": false,
                "isImage": false, "noOutputExpected": false,
            },
            "tool_use_id": "toolu_0142GXHC7zQ9PAiqnZ8Gt66Z",
        })
        .to_string()
    }

    fn answer(printed: &str) -> serde_json::Map<String, Value> {
        let value: Value = serde_json::from_str(printed).expect("the hook prints JSON");
        value["hookSpecificOutput"]["updatedToolOutput"]
            .as_object()
            .expect("the hook answers with the CLI's report shape")
            .clone()
    }

    #[test]
    fn a_trimmed_test_run_is_handed_back_shorter_with_the_whole_of_it_kept() {
        let scratch = scratch();
        let dir = scratch.path().join(DIR);
        let whole = passing_run();

        let printed = hook(call("cargo test", &whole, "").as_bytes(), &dir)
            .expect("a passing run of sixty tests is trimmed");
        let report = answer(&printed);

        let kept = report["stdout"].as_str().expect("stdout is text");
        assert!(kept.starts_with("    Finished `test` profile"));
        assert!(kept.contains("test result: ok. 60 passed"));
        assert!(!kept.contains("... ok\n"));
        let path = dir.join("toolu_0142GXHC7zQ9PAiqnZ8Gt66Z.txt");
        assert!(kept.ends_with(&format!(
            "[niobe left out 61 lines of cargo's build progress and passing tests, {} bytes \
             down to {}. The whole output is in {}]\n",
            whole.len(),
            kept.find("\n[niobe").expect("the note is there"),
            path.display()
        )));
        assert_eq!(
            niobe_core::test_run::counts("cargo test", kept, Some(0)),
            niobe_core::test_run::counts("cargo test", &whole, Some(0)),
            "the shell counts the run from what the agent read, note and all"
        );
        assert_eq!(report["stderr"], "");
        assert_eq!(report["niobeRawOutputPath"], path.display().to_string());
        assert_eq!(report["niobeRawOutputSize"], whole.len());
        assert_eq!(report["interrupted"], false);
        assert_eq!(
            std::fs::read_to_string(&path).expect("the whole output is kept"),
            whole
        );
        assert_eq!(
            std::fs::read_to_string(dir.join(".gitignore")).expect("the directory ignores itself"),
            IGNORE
        );
    }

    #[test]
    fn standard_error_is_trimmed_with_standard_output_in_the_order_the_agent_reads_them() {
        let scratch = scratch();
        let dir = scratch.path().join(DIR);
        let progress: String = (0..40)
            .map(|n| format!("   Compiling crate_number_{n} v1.0.{n}\n"))
            .collect();

        let printed = hook(call("cargo build", "built\n", &progress).as_bytes(), &dir)
            .expect("forty progress lines are trimmed");

        let kept = answer(&printed)["stdout"].as_str().map(str::to_owned);
        assert!(kept.is_some_and(|k| k.starts_with("built\n\n\n[niobe left out 40 lines")));
        let path = dir.join("toolu_0142GXHC7zQ9PAiqnZ8Gt66Z.txt");
        assert_eq!(
            std::fs::read_to_string(path).expect("the whole output is kept"),
            format!("built\n\n{progress}")
        );
    }

    fn aged(path: &Path, days: u64) {
        let file = std::fs::File::options()
            .write(true)
            .open(path)
            .expect("the file was just written");
        let then = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86_400);
        file.set_modified(then).expect("a file's time can be set");
    }

    #[test]
    fn whole_outputs_kept_past_their_time_are_removed_and_nothing_else_is() {
        let scratch = scratch();
        let dir = scratch.path().join(DIR);
        prepare(&dir).expect("the directory is made");
        let old = dir.join("toolu_old.txt");
        let recent = dir.join("toolu_recent.txt");
        let settings = dir.join("claude-settings-max.json");
        let foreign = dir.join("notes.md");
        for path in [&old, &recent, &settings, &foreign] {
            std::fs::write(path, "kept?").expect("a file is written");
        }
        for path in [&old, &settings, &foreign] {
            aged(path, KEEP_DAYS + 1);
        }
        aged(&recent, KEEP_DAYS - 1);
        let ignore = dir.join(".gitignore");
        aged(&ignore, KEEP_DAYS + 1);

        prune(&dir, std::time::SystemTime::now());

        assert!(!old.exists(), "an output past its time was kept");
        for path in [&recent, &settings, &foreign, &ignore] {
            assert!(path.exists(), "{} was removed", path.display());
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_link_named_like_an_output_is_not_followed_or_removed() {
        let scratch = scratch();
        let dir = scratch.path().join(DIR);
        prepare(&dir).expect("the directory is made");
        let target = scratch.path().join("elsewhere.txt");
        std::fs::write(&target, "not niobe's").expect("a file is written");
        aged(&target, KEEP_DAYS + 1);
        let link = dir.join("toolu_link.txt");
        std::os::unix::fs::symlink(&target, &link).expect("a link is made");

        prune(&dir, std::time::SystemTime::now());

        assert!(target.exists());
        assert!(std::fs::symlink_metadata(&link).is_ok());
    }

    #[test]
    fn a_trimmed_call_clears_out_the_outputs_kept_past_their_time() {
        let scratch = scratch();
        let dir = scratch.path().join(DIR);
        prepare(&dir).expect("the directory is made");
        let old = dir.join("toolu_old.txt");
        std::fs::write(&old, "old").expect("a file is written");
        aged(&old, KEEP_DAYS + 1);

        hook(call("cargo test", &passing_run(), "").as_bytes(), &dir).expect("trimmed");

        assert!(!old.exists());
        assert!(dir.join("toolu_0142GXHC7zQ9PAiqnZ8Gt66Z.txt").exists());
    }

    #[test]
    fn a_call_the_hook_cannot_account_for_is_handed_back_as_it_was() {
        let scratch = scratch();
        let dir = scratch.path().join(DIR);
        let whole = passing_run();
        let unchanged = |input: String| hook(input.as_bytes(), &dir);

        assert_eq!(unchanged(call("ls -la", &whole, "")), None, "not cargo");
        assert_eq!(
            unchanged(call("rtk cargo test", &whole, "")),
            None,
            "rtk's already"
        );
        assert_eq!(unchanged(call("cargo test", "ok\n", "")), None, "too short");
        let read = call("cargo test", &whole, "")
            .replace(r#""tool_name":"Bash""#, r#""tool_name":"Read""#);
        assert_eq!(unchanged(read), None, "not a shell call");
        let stopped = call("cargo test", &whole, "")
            .replace(r#""interrupted":false"#, r#""interrupted":true"#);
        assert_eq!(unchanged(stopped), None, "interrupted");
        let escaping =
            call("cargo test", &whole, "").replace("toolu_0142GXHC7zQ9PAiqnZ8Gt66Z", "../../x");
        assert_eq!(unchanged(escaping), None, "an id that is not a file name");
        assert_eq!(unchanged("not json".to_owned()), None, "not a call");
        assert!(!dir.join("toolu_0142GXHC7zQ9PAiqnZ8Gt66Z.txt").exists());
    }

    #[test]
    fn the_hook_is_added_to_a_profiles_own_settings_after_its_hooks() {
        let scratch = scratch();
        let root = scratch.path();
        let own = root.join("own.json");
        std::fs::write(
            &own,
            r#"{"env":{"A":"1"},"hooks":{"PostToolUse":[{"matcher":"Edit","hooks":[]}]}}"#,
        )
        .expect("the profile's settings are written");
        let trim = Trim {
            exe: PathBuf::from("/opt/it's/niobe"),
            dir: root.join(DIR),
        };

        let path = settings(&trim, "max", own.to_str()).expect("the settings are written");

        assert_eq!(
            path,
            trim.dir
                .join("claude-settings-max.json")
                .display()
                .to_string()
        );
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("they can be read"))
                .expect("they are JSON");
        assert_eq!(written["env"]["A"], "1");
        let post = written["hooks"]["PostToolUse"].as_array().expect("a list");
        assert_eq!(post.len(), 2);
        assert_eq!(post[0]["matcher"], "Edit");
        assert_eq!(post[1]["matcher"], "Bash");
        assert_eq!(
            post[1]["hooks"][0]["command"],
            format!(
                r"'/opt/it'\''s/niobe' hook trim --raw '{}'",
                trim.dir.display()
            )
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)
                .expect("there")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn each_profile_has_a_copy_of_its_own_whatever_it_is_called() {
        assert_eq!(settings_name("work-eu"), "claude-settings-work-eu.json");
        assert_eq!(settings_name("../x"), "claude-settings-_2e_2e_2fx.json");
        assert_ne!(settings_name("a_2f"), settings_name("a/"));
    }

    #[test]
    fn a_session_with_no_settings_of_its_own_gets_the_hook_alone() {
        let scratch = scratch();
        let trim = Trim {
            exe: PathBuf::from("/bin/niobe"),
            dir: scratch.path().join(DIR),
        };

        let path = settings(&trim, "max", None).expect("the settings are written");

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("they can be read"))
                .expect("they are JSON");
        assert_eq!(written["hooks"]["PostToolUse"][0]["matcher"], "Bash");
        assert_eq!(written.as_object().map(|o| o.len()), Some(1));
    }

    #[test]
    fn settings_the_hook_cannot_be_added_to_are_an_error_naming_the_file() {
        let scratch = scratch();
        let root = scratch.path();
        let trim = Trim {
            exe: PathBuf::from("/bin/niobe"),
            dir: root.join(DIR),
        };
        let bad = root.join("bad.json");
        std::fs::write(&bad, r#"{"hooks":[]}"#).expect("written");

        let error =
            settings(&trim, "max", bad.to_str()).expect_err("hooks that are a list are refused");

        assert!(error.contains("bad.json"), "{error}");
        assert!(error.contains("`hooks` is not an object"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_that_is_a_link_is_not_written_through() {
        let scratch = scratch();
        let root = scratch.path();
        let other = self::scratch();
        let elsewhere = other.path();
        std::os::unix::fs::symlink(elsewhere, root.join(DIR)).expect("the link is planted");
        let trim = Trim {
            exe: PathBuf::from("/bin/niobe"),
            dir: root.join(DIR),
        };

        assert!(settings(&trim, "max", None).is_err());
        assert_eq!(
            hook(call("cargo test", &passing_run(), "").as_bytes(), &trim.dir),
            None
        );
        assert_eq!(
            std::fs::read_dir(elsewhere).map(|d| d.count()).ok(),
            Some(0)
        );
    }
}
