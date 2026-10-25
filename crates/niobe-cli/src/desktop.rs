// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Copies the text the operator selects, opens the links they click, and
//! opens the memory files they ask to edit in a text editor.
//!
//! What is selected, and what a click opens, is written down in
//! [`niobe_tui::desktop`]; this is the part that runs the programs, which the
//! shell may not.
//!
//! Text goes on the clipboard through the program the platform has for it:
//! `pbcopy` on macOS, `wl-copy`, `xclip` or `xsel` on Linux, the first that is
//! installed. A link is opened with `open` on macOS and `xdg-open` on Linux,
//! and only an `http` or `https` one, or a config file that is a regular file
//! and not a link as it is handed over: the opener runs what a `file:` link
//! or a script names and mounts what an `smb:` one does.
//! Each runs with no terminal — its output and errors dropped — so nothing it
//! says can land on the screen the shell draws on, and in a process group of
//! its own, so a browser it starts is not ended with the session's terminal.
//! None is waited on: a program that started is taken to do what it is for,
//! and is reaped on a thread of its own when it ends.
//!
//! A file to edit is the exception to all of that. It goes to `$VISUAL` or
//! `$EDITOR` — split on whitespace and run with no shell, the path its last
//! argument — on the terminal the shell lends it, in the session's own
//! process group so that it may read the terminal, and is waited on. Where
//! neither is set it goes to `open -t` on macOS, which opens a file in the
//! text editor whatever kind it is, and to `vi` elsewhere. It never goes to
//! the opener: `open` and `xdg-open` dispatch on the file's kind, and run a
//! `.command`, `.tool` or `.desktop` file a repository named rather than show
//! it.

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

use niobe_tui::desktop::Desktop;

/// A program and the arguments it is run with.
#[derive(Debug, Clone)]
struct Program {
    name: String,
    args: Vec<String>,
}

impl Program {
    fn new(name: &str, args: &[&str]) -> Self {
        Self {
            name: name.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        }
    }

    /// The program, started with no terminal and in a group of its own, with
    /// its input piped where `input` says so and closed otherwise.
    fn start(&self, extra: &[&str], input: bool) -> std::io::Result<Child> {
        Command::new(&self.name)
            .args(&self.args)
            .args(extra)
            .stdin(if input { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
    }

    /// The program run on the terminal as it is, with `path` as its last
    /// argument, waited on until it ends.
    fn run_on_the_terminal(&self, path: &str) -> Result<(), String> {
        let status = Command::new(&self.name)
            .args(&self.args)
            .arg(path)
            .status()
            .map_err(|error| format!("{} did not start: {error}", self.name))?;
        match status.success() {
            true => Ok(()),
            false => Err(format!("{} ended with {status}", self.name)),
        }
    }
}

/// The text editor a file is edited in: the first of `visual` and `editor`
/// that names a program, split on whitespace, and otherwise the platform's,
/// `open -t` on macOS and `vi` elsewhere.
fn editor_from(visual: Option<&str>, editor: Option<&str>, macos: bool) -> Program {
    let named = [visual, editor].into_iter().flatten().find_map(|named| {
        let mut words = named.split_whitespace();
        let name = words.next()?;
        Some(Program::new(name, &words.collect::<Vec<_>>()))
    });
    match (named, macos) {
        (Some(named), _) => named,
        (None, true) => Program::new("open", &["-t"]),
        (None, false) => Program::new("vi", &[]),
    }
}

/// The text editor this process's environment names, or the platform's.
fn platform_editor() -> Program {
    let visual = std::env::var("VISUAL").ok();
    let editor = std::env::var("EDITOR").ok();
    editor_from(
        visual.as_deref(),
        editor.as_deref(),
        cfg!(target_os = "macos"),
    )
}

/// The programs that put text on the clipboard on this platform, in the order
/// they are tried.
fn platform_copiers() -> Vec<Program> {
    if cfg!(target_os = "macos") {
        vec![Program::new("pbcopy", &[])]
    } else {
        vec![
            Program::new("wl-copy", &[]),
            Program::new("xclip", &["-selection", "clipboard"]),
            Program::new("xsel", &["--clipboard", "--input"]),
        ]
    }
}

/// The program that opens a link on this platform.
fn platform_opener() -> Program {
    if cfg!(target_os = "macos") {
        Program::new("open", &[])
    } else {
        Program::new("xdg-open", &[])
    }
}

/// The schemes a link is handed to the opener in. Every other scheme is
/// refused, because the opener dispatches on it: `file:` runs an application
/// or a `.command`, `smb:` and `afp:` mount a share and offer it the
/// operator's credentials, and an application's own scheme does what that
/// application lets a link do.
const SCHEMES: [&str; 2] = ["https://", "http://"];

/// The kind of file the opener is handed: a config file, the only file the
/// shell offers to open. Every other kind is refused, since the opener runs
/// a `.command`, `.terminal`, `.fileloc` or `.desktop` file rather than
/// show it.
const OPENED_FILES: &str = "toml";

/// Whether `link` may go to the opener: a web link with nothing in it that is
/// not printable ASCII, so what opens is what was drawn, or the whole path of
/// a config file that is, now, a regular file and not a link.
///
/// The shell hands over only such things, but it looked at a file when the
/// session started, and the repository can have replaced it since — a branch
/// switched mid-session can make a committed config file a link to a script.
/// So it is looked at again here, as it is handed over.
fn may_open(link: &str) -> Result<(), String> {
    if !link.chars().all(|c| c.is_ascii() && !c.is_ascii_control()) {
        return Err("it holds characters a link is not drawn with".to_owned());
    }
    if SCHEMES.iter().any(|scheme| link.starts_with(scheme)) {
        return Ok(());
    }
    let path = std::path::Path::new(link);
    if !path.is_absolute() {
        return Err("only a web link or a config file is opened".to_owned());
    }
    if path.extension().and_then(std::ffi::OsStr::to_str) != Some(OPENED_FILES) {
        return Err("only a config file is opened".to_owned());
    }
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_file() => Ok(()),
        Ok(_) => Err("it is no longer a file, but a link or a directory".to_owned()),
        Err(error) => Err(format!("it cannot be read: {error}")),
    }
}

/// The desktop the session runs on.
#[derive(Debug)]
pub struct System {
    copiers: Vec<Program>,
    opener: Program,
    editor: Program,
    diagnostics: Option<Diagnostics>,
}

/// What makes the diagnostics block of a report of a problem. Shared with
/// the thread that gathers it, since gathering asks the CLIs' own status
/// commands, which take seconds.
struct Diagnostics(std::sync::Arc<dyn Fn() -> String + Send + Sync>);

impl std::fmt::Debug for Diagnostics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Diagnostics")
    }
}

impl System {
    /// The programs this platform has for the clipboard and for links, and
    /// the text editor the environment names.
    pub fn new() -> Self {
        Self {
            copiers: platform_copiers(),
            opener: platform_opener(),
            editor: platform_editor(),
            diagnostics: None,
        }
    }

    /// This desktop, able to put a report's diagnostics on the clipboard:
    /// `diagnostics` makes the block when a report is asked for.
    #[must_use]
    pub fn reporting(mut self, diagnostics: impl Fn() -> String + Send + Sync + 'static) -> Self {
        self.diagnostics = Some(Diagnostics(std::sync::Arc::new(diagnostics)));
        self
    }

    /// The first copier that is installed, started with its input piped,
    /// or why none could be.
    fn start_copier(&self) -> Result<Child, String> {
        for copier in &self.copiers {
            match copier.start(&[], true) {
                Ok(child) => return Ok(child),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("{} did not start: {error}", copier.name)),
            }
        }
        let names: Vec<&str> = self.copiers.iter().map(|c| c.name.as_str()).collect();
        Err(format!("this needs one of {} installed", names.join(", ")))
    }
}

/// Writes `text` to `copier`'s input, closes it and waits for the copier to
/// end, saying whether it ended successfully.
fn hand_to(mut copier: Child, text: &str) -> bool {
    if let Some(mut input) = copier.stdin.take() {
        let _ = input.write_all(text.as_bytes());
    }
    copier.wait().is_ok_and(|status| status.success())
}

impl Desktop for System {
    fn copy(&mut self, text: &str) -> Result<(), String> {
        let copier = self.start_copier()?;
        let text = text.to_owned();
        // Written on the thread that reaps it, so a copier slow to read
        // does not hold up the frame.
        std::thread::spawn(move || hand_to(copier, &text));
        Ok(())
    }

    fn open(&mut self, link: &str) -> Result<(), String> {
        may_open(link)?;
        let mut child = self
            .opener
            .start(&[link], false)
            .map_err(|error| format!("{} did not start: {error}", self.opener.name))?;
        std::thread::spawn(move || child.wait());
        Ok(())
    }

    fn edit(&mut self, path: &str) -> Result<(), String> {
        // The memory view hands over a whole path, which cannot read as an
        // option; anything else is refused rather than passed to the editor.
        if !std::path::Path::new(path).is_absolute() {
            return Err("it is not a file's whole path".to_owned());
        }
        self.editor.run_on_the_terminal(path)
    }

    fn report(&mut self, link: &str) -> Result<(), String> {
        let Some(diagnostics) = &self.diagnostics else {
            return Err("this session has no diagnostics to put in a report".to_owned());
        };
        may_open(link)?;
        let copier = self.start_copier().map_err(|reason| {
            format!("{reason}; `niobe doctor --json` prints the same block to paste")
        })?;
        let gather = std::sync::Arc::clone(&diagnostics.0);
        let opener = self.opener.clone();
        let link = link.to_owned();
        // Gathered off the draw path: the block asks each CLI for its version
        // and its login, each given seconds to answer, and reads the store.
        // The form is opened only once the block is on the clipboard, since
        // a form the operator cannot paste into asks for the very thing they
        // were not given.
        std::thread::spawn(move || {
            if hand_to(copier, &gather())
                && let Ok(mut opened) = opener.start(&[&link], false)
            {
                let _ = opened.wait();
            }
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    /// How long a test waits for a program it started to write what it
    /// writes. Starting `sh` takes seconds on a loaded Mac, and ten of them
    /// have run out there; the wait returns as soon as the file is written.
    const PATIENCE: Duration = Duration::from_secs(60);

    /// A directory of its own under the system's temporary one, removed when
    /// dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "niobe-desktop-{name}-{}-{:?}",
                std::process::id(),
                Instant::now()
            ));
            std::fs::create_dir_all(&dir).expect("the temporary directory is writable");
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn sh(script: &str) -> Program {
        Program::new("sh", &["-c", script, "sh"])
    }

    /// What `path` holds once something has written it, waiting a while for
    /// the program writing it on another thread.
    fn written(path: &std::path::Path) -> String {
        let started = Instant::now();
        while started.elapsed() < PATIENCE {
            if let Ok(text) = std::fs::read_to_string(path)
                && !text.is_empty()
            {
                return text;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("nothing was written to {}", path.display());
    }

    #[test]
    fn text_is_handed_to_the_first_copier_that_is_installed() {
        let scratch = Scratch::new("copy");
        let out = scratch.0.join("copied");
        let mut desktop = System {
            copiers: vec![
                Program::new("niobe-no-such-copier", &[]),
                sh(&format!(
                    "cat > '{0}.part' && mv '{0}.part' '{0}'",
                    out.display()
                )),
            ],
            opener: sh("true"),
            editor: sh("true"),
            diagnostics: None,
        };
        assert_eq!(desktop.copy("line one\nline two"), Ok(()));
        assert_eq!(written(&out), "line one\nline two");
    }

    #[test]
    fn a_clipboard_with_no_copier_installed_says_which_it_needs() {
        let mut desktop = System {
            copiers: vec![Program::new("niobe-no-such-copier", &[])],
            opener: sh("true"),
            editor: sh("true"),
            diagnostics: None,
        };
        let refused = desktop.copy("text").expect_err("nothing can copy it");
        assert!(refused.contains("niobe-no-such-copier"), "{refused}");
    }

    #[test]
    fn a_link_is_handed_to_the_opener_as_one_argument() {
        let scratch = Scratch::new("open");
        let out = scratch.0.join("opened");
        let mut desktop = System {
            copiers: Vec::new(),
            opener: sh(&format!("printf '%s' \"$1\" > '{}'", out.display())),
            editor: sh("true"),
            diagnostics: None,
        };
        assert_eq!(desktop.open("https://example.com/a b"), Ok(()));
        assert_eq!(written(&out), "https://example.com/a b");
    }

    #[test]
    fn an_argument_that_would_read_as_an_option_is_not_opened() {
        let mut desktop = System {
            copiers: Vec::new(),
            opener: sh("true"),
            editor: sh("true"),
            diagnostics: None,
        };
        assert!(desktop.open("--help").is_err());
    }

    #[test]
    fn a_link_in_a_scheme_that_runs_a_program_or_mounts_a_share_is_not_opened() {
        let scratch = Scratch::new("schemes");
        let out = scratch.0.join("opened");
        let mut desktop = System {
            copiers: Vec::new(),
            opener: sh(&format!("printf '%s' \"$1\" > '{}'", out.display())),
            editor: sh("true"),
            diagnostics: None,
        };
        for link in [
            "file:///Applications/Calculator.app",
            "smb://attacker.example/share",
            "afp://attacker.example/share",
            "vscode://file/etc/passwd",
            "x-apple.systempreferences:com.apple.preference.security",
            "ssh://attacker.example",
            "tel:+15555550100",
            "javascript:alert(1)",
            "HTTPS://example.com",
            "https://example.com/\u{1b}]8;;smb://x\u{7}",
            "https://example.com/\u{202e}gpj.command",
            "tools/setup.command",
            "-a Terminal",
        ] {
            assert!(desktop.open(link).is_err(), "{link:?} was handed over");
        }
        assert!(!out.exists(), "the opener ran");
    }

    #[test]
    fn a_file_is_opened_only_where_it_is_a_config_file_and_not_a_link_when_it_is_asked_for() {
        let scratch = Scratch::new("files");
        let out = scratch.0.join("opened");
        let mut desktop = System {
            copiers: Vec::new(),
            opener: sh(&format!("printf '%s' \"$1\" > '{}'", out.display())),
            editor: sh("true"),
            diagnostics: None,
        };
        let script = scratch.0.join("setup.command");
        std::fs::write(&script, "#!/bin/sh\n").expect("the scratch directory is writable");
        assert!(desktop.open(&script.display().to_string()).is_err());

        // Looked at when the session started, a regular file; replaced by a
        // link to something the opener runs by the time the key is pressed,
        // as a branch switched mid-session can replace it.
        let config = scratch.0.join("config.toml");
        std::os::unix::fs::symlink(&script, &config).expect("a link can be made");
        assert!(desktop.open(&config.display().to_string()).is_err());
        assert!(!out.exists(), "the opener ran");

        std::fs::remove_file(&config).expect("the link is removed");
        std::fs::write(&config, "theme = \"cyber\"\n").expect("the scratch directory is writable");
        assert_eq!(desktop.open(&config.display().to_string()), Ok(()));
        assert_eq!(written(&out), config.display().to_string());
    }

    fn named(program: &Program) -> (String, Vec<String>) {
        (program.name.clone(), program.args.clone())
    }

    #[test]
    fn a_file_is_edited_in_the_editor_the_environment_names_and_never_by_the_opener() {
        assert_eq!(
            named(&editor_from(Some("code --wait"), Some("vim"), true)),
            ("code".to_owned(), vec!["--wait".to_owned()])
        );
        assert_eq!(
            named(&editor_from(Some("  "), Some("nvim"), false)),
            ("nvim".to_owned(), Vec::new())
        );
        assert_eq!(
            named(&editor_from(None, None, true)),
            ("open".to_owned(), vec!["-t".to_owned()]),
            "macOS's opener is asked for its text editor, whatever the file's kind"
        );
        assert_eq!(
            named(&editor_from(None, None, false)),
            ("vi".to_owned(), Vec::new())
        );
        for program in [
            editor_from(None, None, true),
            editor_from(None, None, false),
        ] {
            let run = named(&program);
            assert_ne!(run, ("open".to_owned(), Vec::new()));
            assert_ne!(run, ("xdg-open".to_owned(), Vec::new()));
        }
    }

    #[test]
    fn a_file_to_edit_is_the_editors_last_argument_and_is_waited_on() {
        let scratch = Scratch::new("edit");
        let out = scratch.0.join("edited");
        let mut desktop = System {
            copiers: Vec::new(),
            opener: sh("exit 1"),
            editor: sh(&format!("printf '%s' \"$1\" > '{}'", out.display())),
            diagnostics: None,
        };
        let path = scratch.0.join("tools").join("setup.command");
        assert_eq!(desktop.edit(&path.display().to_string()), Ok(()));
        // Waited on: written by the time the edit returns.
        assert_eq!(
            std::fs::read_to_string(&out).expect("the editor ran before edit returned"),
            path.display().to_string()
        );
    }

    #[test]
    fn an_editor_that_fails_or_a_path_that_is_not_whole_says_so() {
        let mut desktop = System {
            copiers: Vec::new(),
            opener: sh("true"),
            editor: sh("exit 3"),
            diagnostics: None,
        };
        let failed = desktop
            .edit("/work/CLAUDE.md")
            .expect_err("the editor failed");
        assert!(failed.contains("sh ended"), "{failed}");
        assert!(desktop.edit("--help").is_err());
        assert!(desktop.edit("CLAUDE.md").is_err());
    }

    #[test]
    fn a_report_returns_before_its_diagnostics_are_gathered_and_opens_the_form_once_they_are_copied()
     {
        let scratch = Scratch::new("report");
        let copied = scratch.0.join("copied");
        let opened = scratch.0.join("opened");
        // The diagnostics wait for a word the test sends only once `report`
        // has returned, as gathering them waits on the CLIs' status commands,
        // which take seconds: a report that gathered them on the draw path
        // would hold the screen until they gave up.
        let (go, gathering) = std::sync::mpsc::channel::<()>();
        let gathering = std::sync::Mutex::new(gathering);
        let mut desktop = System {
            copiers: vec![sh(&format!(
                "cat > '{0}.part' && mv '{0}.part' '{0}'",
                copied.display()
            ))],
            opener: sh(&format!(
                "test -f '{}' && printf '%s' \"$1\" > '{}'",
                copied.display(),
                opened.display()
            )),
            editor: sh("true"),
            diagnostics: None,
        }
        .reporting(move || {
            let waited = gathering
                .lock()
                .map(|gathering| gathering.recv_timeout(Duration::from_secs(5)));
            match waited {
                Ok(Ok(())) => "the diagnostics".to_owned(),
                _ => "gathered while the screen waited".to_owned(),
            }
        });

        assert_eq!(desktop.report("https://example.com/issues/new"), Ok(()));
        let _ = go.send(());

        assert_eq!(written(&copied), "the diagnostics");
        assert_eq!(written(&opened), "https://example.com/issues/new");
    }

    #[test]
    fn a_report_is_refused_by_a_desktop_that_cannot_make_the_diagnostics() {
        let mut desktop = System::new();

        let said = desktop
            .report("https://example.invalid/issues/new")
            .expect_err("there is nothing to put in a report");

        assert!(said.contains("no diagnostics"), "{said}");
    }
}
