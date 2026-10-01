// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Copies the text the operator selects and opens the links they click.
//!
//! What is selected, and what a click opens, is written down in
//! [`niobe_tui::desktop`]; this is the part that runs the programs, which the
//! shell may not.
//!
//! Text goes on the clipboard through the program the platform has for it:
//! `pbcopy` on macOS, `wl-copy`, `xclip` or `xsel` on Linux, the first that is
//! installed. A link is opened with `open` on macOS and `xdg-open` on Linux.
//! Each runs with no terminal — its output and errors dropped — so nothing it
//! says can land on the screen the shell draws on, and in a process group of
//! its own, so a browser it starts is not ended with the session's terminal.
//! None is waited on: a program that started is taken to do what it is for,
//! and is reaped on a thread of its own when it ends.

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

/// The desktop the session runs on.
#[derive(Debug)]
pub struct System {
    copiers: Vec<Program>,
    opener: Program,
}

impl System {
    /// The programs this platform has for the clipboard and for links.
    pub fn new() -> Self {
        Self {
            copiers: platform_copiers(),
            opener: platform_opener(),
        }
    }
}

impl Desktop for System {
    fn copy(&mut self, text: &str) -> Result<(), String> {
        for copier in &self.copiers {
            let mut child = match copier.start(&[], true) {
                Ok(child) => child,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("{} did not start: {error}", copier.name)),
            };
            let mut input = child.stdin.take();
            let text = text.to_owned();
            // Written on the thread that reaps it, so a copier slow to read
            // does not hold up the frame.
            std::thread::spawn(move || {
                if let Some(input) = input.as_mut() {
                    let _ = input.write_all(text.as_bytes());
                }
                drop(input);
                let _ = child.wait();
            });
            return Ok(());
        }
        let names: Vec<&str> = self.copiers.iter().map(|c| c.name.as_str()).collect();
        Err(format!("this needs one of {} installed", names.join(", ")))
    }

    fn open(&mut self, link: &str) -> Result<(), String> {
        // A link is all the shell hands over, but an argument that starts
        // with a dash would be read as an option, whatever it came from.
        if link.starts_with('-') {
            return Err("it is not a link".to_owned());
        }
        let mut child = self
            .opener
            .start(&[link], false)
            .map_err(|error| format!("{} did not start: {error}", self.opener.name))?;
        std::thread::spawn(move || child.wait());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;
    use std::time::{Duration, Instant};

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
        while started.elapsed() < Duration::from_secs(10) {
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
        };
        assert_eq!(desktop.copy("line one\nline two"), Ok(()));
        assert_eq!(written(&out), "line one\nline two");
    }

    #[test]
    fn a_clipboard_with_no_copier_installed_says_which_it_needs() {
        let mut desktop = System {
            copiers: vec![Program::new("niobe-no-such-copier", &[])],
            opener: sh("true"),
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
        };
        assert_eq!(desktop.open("https://example.com/a b"), Ok(()));
        assert_eq!(written(&out), "https://example.com/a b");
    }

    #[test]
    fn an_argument_that_would_read_as_an_option_is_not_opened() {
        let mut desktop = System {
            copiers: Vec::new(),
            opener: sh("true"),
        };
        assert!(desktop.open("--help").is_err());
    }
}
