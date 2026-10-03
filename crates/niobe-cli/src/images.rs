// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Fetches the images the operator attaches to a prompt.
//!
//! How an image is attached, and what it becomes in the prompt, is written
//! down in [`niobe_tui::images`]; this is the part that reads the clipboard
//! and the disk, which the shell may not.
//!
//! The clipboard is read by the programs the platform has for it, as Claude
//! Code reads it: `osascript` on macOS, `wl-paste` or `xclip` on Linux. Each
//! runs with no terminal — its input closed, its output read here and its
//! errors dropped — so nothing it says can land on the screen the shell draws
//! on, and each is killed if it has not finished in [`PATIENCE`].
//!
//! An image larger than a model takes is shrunk on macOS with `sips` — to a
//! JPEG no longer than the first of [`SHRUNK_EDGES`] on its long side that
//! brings it under the limit — before it is refused, since a full-screen screenshot on a high-density display is
//! routinely over the limit and a screenshot is the image most often pasted.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use niobe_core::image::{Image, MAX_ENCODED_BYTES, MediaType, encoded_len};
use niobe_tui::images::{Fetched, Images, Source};

/// How long a program reading the clipboard, or shrinking an image, is given
/// before it is killed and the fetch fails. `osascript` answers in about a
/// tenth of a second; one that has not answered in this long is waiting on
/// something that will not come.
const PATIENCE: Duration = Duration::from_secs(10);

/// The most of a file that is read as an image. Far past anything a model
/// takes even shrunk, and small enough that a path to `/dev/zero`, or to a
/// file that is not an image at all, cannot fill the memory.
const MOST_READ: u64 = 64 * 1024 * 1024;

/// The long edges an image too large to send is shrunk to, tried in turn
/// until one fits. The first is the most a current model reads an image at,
/// so shrinking to it costs nothing the model would have seen. It is not
/// always enough: a 3000×2000 image of noise shrunk to it by `sips` came to
/// 4.87 MB encoded, a hair under the limit, and a busier image lands over it,
/// so smaller ones follow.
const SHRUNK_EDGES: &[u32] = &[2576, 1568, 1024];

/// Where in a shrinking program's arguments the long edge goes.
const EDGE: &str = "{edge}";

/// Where a program that reads the clipboard puts what it read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Output {
    /// Its standard output is the image.
    Bytes,
    /// It writes the image to the file named where its arguments say
    /// [`FILE`].
    File,
    /// Its standard output names a file, which is read as an image dropped
    /// there would be.
    Path,
}

/// Where in a program's arguments the file it is to write goes.
const FILE: &str = "{file}";

/// Where in a shrinking program's arguments the image it shrinks goes.
const INPUT: &str = "{input}";

/// A program that reads an image off the clipboard.
#[derive(Debug, Clone)]
struct Reader {
    program: String,
    args: Vec<String>,
    output: Output,
}

impl Reader {
    fn new(program: &str, args: &[&str], output: Output) -> Self {
        Self {
            program: program.to_owned(),
            args: args.iter().map(|&arg| arg.to_owned()).collect(),
            output,
        }
    }
}

/// The programs this platform reads a clipboard image with, in the order they
/// are tried.
fn platform_readers() -> Vec<Reader> {
    if cfg!(target_os = "macos") {
        vec![
            Reader::new(
                "osascript",
                &[
                    "-e",
                    "set png_data to (the clipboard as «class PNGf»)",
                    "-e",
                    "set fp to open for access POSIX file \"{file}\" with write permission",
                    "-e",
                    "write png_data to fp",
                    "-e",
                    "close access fp",
                ],
                Output::File,
            ),
            // A file copied in the Finder is on the clipboard as its address,
            // not as an image.
            Reader::new(
                "osascript",
                &["-e", "get POSIX path of (the clipboard as «class furl»)"],
                Output::Path,
            ),
        ]
    } else {
        vec![
            Reader::new(
                "wl-paste",
                &["--no-newline", "--type", "image/png"],
                Output::Bytes,
            ),
            Reader::new(
                "xclip",
                &["-selection", "clipboard", "-t", "image/png", "-o"],
                Output::Bytes,
            ),
        ]
    }
}

/// The images the operator attaches, fetched on threads of their own.
#[derive(Debug)]
pub struct Clipboard {
    fetcher: Fetcher,
    ended: Receiver<Fetched>,
    ends: Sender<Fetched>,
}

/// Everything a fetch needs, cloned onto the thread that does it.
#[derive(Debug, Clone)]
struct Fetcher {
    /// What a relative path is relative to: where the session runs.
    root: PathBuf,
    /// What `~` stands for, where it is known.
    home: Option<PathBuf>,
    readers: Vec<Reader>,
    /// What shrinks an image too large to send, where this platform has
    /// something: it reads [`INPUT`] and writes [`FILE`].
    shrinker: Option<Reader>,
}

/// What shrinks an image on this platform, where there is something.
fn platform_shrinker() -> Option<Reader> {
    cfg!(target_os = "macos").then(|| {
        Reader::new(
            "sips",
            &[
                "-s",
                "format",
                "jpeg",
                "-s",
                "formatOptions",
                "80",
                "-Z",
                EDGE,
                INPUT,
                "--out",
                FILE,
            ],
            Output::File,
        )
    })
}

impl Clipboard {
    /// Fetches for a session run at `root`, by an operator whose home is
    /// `home`.
    pub fn at(root: &Path, home: Option<PathBuf>) -> Self {
        Self::with(Fetcher {
            root: root.to_path_buf(),
            home,
            readers: platform_readers(),
            shrinker: platform_shrinker(),
        })
    }

    fn with(fetcher: Fetcher) -> Self {
        let (ends, ended) = channel();
        Self {
            fetcher,
            ended,
            ends,
        }
    }
}

impl Images for Clipboard {
    fn fetch(&mut self, source: &Source) {
        let fetcher = self.fetcher.clone();
        let ends = self.ends.clone();
        let source = source.clone();
        std::thread::spawn(move || {
            let image = fetcher.fetch(&source);
            // The receiver goes only with the session, and then nobody is
            // waiting on the image.
            let _ = ends.send(Fetched { source, image });
        });
    }

    fn drain(&mut self) -> Vec<Fetched> {
        self.ended.try_iter().collect()
    }
}

impl Fetcher {
    fn fetch(&self, source: &Source) -> Result<Image, String> {
        let bytes = match source {
            Source::Clipboard => self.clipboard()?,
            Source::File(path) => read_image_file(&self.resolve(path))?,
        };
        self.image(bytes)
    }

    /// The image in `bytes`, shrunk first where it is too large to send and
    /// this platform can shrink it.
    fn image(&self, bytes: Vec<u8>) -> Result<Image, String> {
        let too_large = encoded_len(bytes.len()) > MAX_ENCODED_BYTES;
        let bytes = match &self.shrinker {
            Some(shrinker) if too_large && MediaType::sniff(&bytes).is_some() => {
                // Where shrinking fails, the image as it was is refused for
                // its size, which is the reason the operator can act on.
                shrink(shrinker, &bytes).unwrap_or(bytes)
            }
            _ => bytes,
        };
        Image::from_bytes(bytes).map_err(|error| error.to_string())
    }

    /// The path `pasted` names, as the session's directory and the
    /// operator's home read it.
    fn resolve(&self, pasted: &str) -> PathBuf {
        match (pasted.strip_prefix("~/"), &self.home) {
            (Some(rest), Some(home)) => home.join(rest),
            _ => self.root.join(pasted),
        }
    }

    /// The image on the clipboard, from the first reader that has one.
    fn clipboard(&self) -> Result<Vec<u8>, String> {
        let mut installed = false;
        for reader in &self.readers {
            match read_with(reader) {
                Ok(Some(bytes)) => return Ok(bytes),
                Ok(None) => installed = true,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => installed = true,
            }
        }
        if installed || self.readers.is_empty() {
            Err("there is no image on the clipboard".to_owned())
        } else {
            let names = self
                .readers
                .iter()
                .map(|reader| format!("`{}`", reader.program))
                .collect::<Vec<_>>();
            Err(format!(
                "reading the clipboard needs {}, and none is installed",
                names.join(" or ")
            ))
        }
    }
}

/// What `reader` read off the clipboard: `None` where it ran and found no
/// image there.
fn read_with(reader: &Reader) -> std::io::Result<Option<Vec<u8>>> {
    let scratch = Scratch::new("png");
    let args = reader
        .args
        .iter()
        .map(|arg| arg.replace(FILE, &scratch.path().display().to_string()));
    let (succeeded, stdout) = run(Command::new(&reader.program).args(args))?;
    if !succeeded {
        return Ok(None);
    }
    let bytes = match reader.output {
        Output::Bytes => stdout,
        Output::File => read_capped(scratch.path())?,
        Output::Path => {
            let named = String::from_utf8_lossy(&stdout);
            let path = named.trim_end_matches(['\n', '\r']);
            if !niobe_tui::images::names_an_image(path) {
                return Ok(None);
            }
            read_capped(Path::new(path))?
        }
    };
    Ok((!bytes.is_empty()).then_some(bytes))
}

/// `bytes`, an image, as `shrinker` shrinks it to the first of
/// [`SHRUNK_EDGES`] that brings it within what a model takes; `None` where
/// none does, or it could not be shrunk at all.
fn shrink(shrinker: &Reader, bytes: &[u8]) -> Option<Vec<u8>> {
    let input = Scratch::new("image");
    std::fs::write(input.path(), bytes).ok()?;
    SHRUNK_EDGES.iter().find_map(|&edge| {
        let shrunk = shrink_to(shrinker, input.path(), edge)?;
        (encoded_len(shrunk.len()) <= MAX_ENCODED_BYTES).then_some(shrunk)
    })
}

/// The image at `input` as `shrinker` shrinks it to `edge` on its long side.
fn shrink_to(shrinker: &Reader, input: &Path, edge: u32) -> Option<Vec<u8>> {
    let output = Scratch::new("jpg");
    let args = shrinker.args.iter().map(|arg| {
        arg.replace(INPUT, &input.display().to_string())
            .replace(FILE, &output.path().display().to_string())
            .replace(EDGE, &edge.to_string())
    });
    let (succeeded, _) = run(Command::new(&shrinker.program).args(args)).ok()?;
    succeeded.then(|| read_capped(output.path()).ok()).flatten()
}

/// Runs `command` with no terminal, and says whether it succeeded and what it
/// wrote to its standard output. Killed, and failed, after [`PATIENCE`].
fn run(command: &mut Command) -> std::io::Result<(bool, Vec<u8>)> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    // Read on a thread of its own, so an image larger than the pipe holds
    // does not stop the program before it can end.
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("the program's output was not piped"))?;
    let reading = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .by_ref()
            .take(MOST_READ)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() > PATIENCE {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("it had not finished after {} s", PATIENCE.as_secs()),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let bytes = reading
        .join()
        .map_err(|_| std::io::Error::other("reading the program's output failed"))??;
    Ok((status.success(), bytes))
}

/// The bytes of the image file at `path`, in words the operator reads where
/// there are none.
///
/// Only a regular file is read, and only up to [`MOST_READ`]: a path to a
/// pipe would wait for ever, and one to a device would never end.
fn read_image_file(path: &Path) -> Result<Vec<u8>, String> {
    let shown = path.display();
    let metadata =
        std::fs::metadata(path).map_err(|error| format!("cannot read {shown}: {error}"))?;
    if !metadata.is_file() {
        return Err(format!("{shown} is not a file"));
    }
    read_capped(path).map_err(|error| format!("cannot read {shown}: {error}"))
}

/// Up to [`MOST_READ`] bytes of the file at `path`.
fn read_capped(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MOST_READ)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// A file in the temporary directory that is removed when it goes, whether or
/// not anything was written to it.
struct Scratch(PathBuf);

impl Scratch {
    fn new(extension: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        Self(std::env::temp_dir().join(format!(
            "niobe-image-{}-{n}.{extension}",
            std::process::id()
        )))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    fn fetcher(root: &Path, readers: Vec<Reader>) -> Fetcher {
        Fetcher {
            root: root.to_path_buf(),
            home: Some(PathBuf::from("/home/me")),
            readers,
            shrinker: None,
        }
    }

    /// A stand-in clipboard reader: `sh` running `script`, with the file it
    /// is to write, where it writes one, as `$1`.
    fn sh(script: &str, output: Output) -> Reader {
        Reader {
            program: "sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                script.to_owned(),
                "sh".to_owned(),
                FILE.to_owned(),
            ],
            output,
        }
    }

    fn wrote_png(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, PNG).expect("the image is written");
        path
    }

    #[test]
    fn a_pasted_path_is_read_where_the_session_and_the_operator_would_read_it() {
        let fetcher = fetcher(Path::new("/repo"), Vec::new());
        assert_eq!(fetcher.resolve("shot.png"), PathBuf::from("/repo/shot.png"));
        assert_eq!(
            fetcher.resolve("/tmp/shot.png"),
            PathBuf::from("/tmp/shot.png")
        );
        assert_eq!(
            fetcher.resolve("~/shot.png"),
            PathBuf::from("/home/me/shot.png")
        );
    }

    #[test]
    fn an_image_file_is_read_and_anything_else_is_refused_in_words() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let fetcher = fetcher(dir.path(), Vec::new());
        wrote_png(dir.path(), "shot.png");
        std::fs::write(dir.path().join("notes.png"), "text").expect("the file is written");

        let image = fetcher
            .fetch(&Source::File("shot.png".to_owned()))
            .expect("a PNG is an image");
        assert_eq!(image.data(), PNG);

        assert_eq!(
            fetcher.fetch(&Source::File("notes.png".to_owned())),
            Err("it is not a PNG, JPEG, GIF or WebP image".to_owned())
        );
        let missing = fetcher
            .fetch(&Source::File("gone.png".to_owned()))
            .expect_err("there is no such file");
        assert!(missing.starts_with("cannot read "), "{missing}");
        std::fs::create_dir(dir.path().join("folder.png")).expect("the directory is made");
        let folder = fetcher
            .fetch(&Source::File("folder.png".to_owned()))
            .expect_err("a directory is not an image");
        assert!(folder.ends_with("folder.png is not a file"), "{folder}");
    }

    #[test]
    fn the_clipboard_is_read_from_the_first_reader_that_has_an_image() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let image = wrote_png(dir.path(), "copied.png");
        let fetcher = fetcher(
            dir.path(),
            vec![
                sh("exit 1", Output::Bytes),
                sh(&format!("cat '{}'", image.display()), Output::Bytes),
            ],
        );

        let read = fetcher
            .fetch(&Source::Clipboard)
            .expect("the second reader has one");
        assert_eq!(read.data(), PNG);
    }

    #[test]
    fn a_reader_that_writes_a_file_and_one_that_names_a_file_are_both_read() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let image = wrote_png(dir.path(), "copied.png");

        let writes = fetcher(
            dir.path(),
            vec![sh(
                &format!("cp '{}' \"$1\"", image.display()),
                Output::File,
            )],
        );
        assert_eq!(
            writes.fetch(&Source::Clipboard).map(|i| i.data().to_vec()),
            Ok(PNG.to_vec())
        );

        let names = fetcher(
            dir.path(),
            vec![sh(&format!("echo '{}'", image.display()), Output::Path)],
        );
        assert_eq!(
            names.fetch(&Source::Clipboard).map(|i| i.data().to_vec()),
            Ok(PNG.to_vec())
        );
    }

    #[test]
    fn a_file_named_on_the_clipboard_is_read_by_its_name_as_written() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let image = wrote_png(dir.path(), r"Screen Shot \1.png");
        let fetcher = fetcher(
            dir.path(),
            vec![sh(
                &format!("printf '%s\\n' '{}'", image.display()),
                Output::Path,
            )],
        );
        assert_eq!(
            fetcher.fetch(&Source::Clipboard).map(|i| i.data().to_vec()),
            Ok(PNG.to_vec())
        );
    }

    #[test]
    fn a_file_named_on_the_clipboard_that_is_no_image_is_no_image_on_the_clipboard() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let fetcher = fetcher(dir.path(), vec![sh("echo /etc/hosts", Output::Path)]);
        assert_eq!(
            fetcher.fetch(&Source::Clipboard),
            Err("there is no image on the clipboard".to_owned())
        );
    }

    #[test]
    fn a_clipboard_with_no_reader_installed_says_which_it_needs() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let fetcher = fetcher(
            dir.path(),
            vec![
                Reader::new("niobe-no-such-reader-a", &[], Output::Bytes),
                Reader::new("niobe-no-such-reader-b", &[], Output::Bytes),
            ],
        );
        assert_eq!(
            fetcher.fetch(&Source::Clipboard),
            Err("reading the clipboard needs `niobe-no-such-reader-a` or \
                 `niobe-no-such-reader-b`, and none is installed"
                .to_owned())
        );
    }

    /// A PNG whose encoding is just over what a model takes.
    fn too_large() -> Vec<u8> {
        let mut data = PNG.to_vec();
        data.resize(MAX_ENCODED_BYTES / 4 * 3 + 1, 0);
        data
    }

    #[test]
    fn an_image_too_large_to_send_is_shrunk_where_something_can_shrink_it() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let small = wrote_png(dir.path(), "small.png");
        let big = dir.path().join("big.png");
        std::fs::write(&big, too_large()).expect("the image is written");
        let mut fetcher = fetcher(dir.path(), Vec::new());
        fetcher.shrinker = Some(Reader {
            program: "sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                format!("test -s \"$1\" && cp '{}' \"$2\"", small.display()),
                "sh".to_owned(),
                INPUT.to_owned(),
                FILE.to_owned(),
            ],
            output: Output::File,
        });

        let image = fetcher
            .fetch(&Source::File("big.png".to_owned()))
            .expect("the shrunk image fits");
        assert_eq!(image.data(), PNG);
    }

    #[test]
    fn an_image_still_too_large_at_one_edge_is_shrunk_further() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let small = wrote_png(dir.path(), "small.png");
        let big = dir.path().join("big.png");
        std::fs::write(&big, too_large()).expect("the image is written");
        let tried = dir.path().join("tried");
        let mut fetcher = fetcher(dir.path(), Vec::new());
        // Hands back the image as large as it came until the last edge.
        fetcher.shrinker = Some(Reader {
            program: "sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                format!(
                    "echo \"$3\" >> '{}'; if [ \"$3\" = 1024 ]; then cp '{}' \"$2\"; \
                     else cp \"$1\" \"$2\"; fi",
                    tried.display(),
                    small.display()
                ),
                "sh".to_owned(),
                INPUT.to_owned(),
                FILE.to_owned(),
                EDGE.to_owned(),
            ],
            output: Output::File,
        });

        let image = fetcher
            .fetch(&Source::File("big.png".to_owned()))
            .expect("the last edge fits");

        assert_eq!(image.data(), PNG);
        let tried = std::fs::read_to_string(&tried).expect("the edges were recorded");
        assert_eq!(tried.lines().collect::<Vec<_>>(), ["2576", "1568", "1024"]);
    }

    #[test]
    fn an_image_too_large_to_send_is_refused_for_its_size_where_nothing_shrinks_it() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        std::fs::write(dir.path().join("big.png"), too_large()).expect("the image is written");
        let mut fetcher = fetcher(dir.path(), Vec::new());
        let refused = fetcher
            .fetch(&Source::File("big.png".to_owned()))
            .expect_err("nothing shrinks it");
        assert!(
            refused.starts_with("the image is 5.0 MB encoded"),
            "{refused}"
        );

        fetcher.shrinker = Some(Reader::new("false", &[], Output::File));
        assert_eq!(
            fetcher.fetch(&Source::File("big.png".to_owned())),
            Err(refused),
            "a shrinker that fails leaves the size as the reason"
        );
    }

    #[test]
    fn a_reader_leaves_no_file_behind() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let record = dir.path().join("wrote-to");
        let fetcher = fetcher(
            dir.path(),
            vec![sh(
                &format!("echo \"$1\" > '{}'; echo x > \"$1\"", record.display()),
                Output::File,
            )],
        );

        let _ = fetcher.fetch(&Source::Clipboard);

        let written = std::fs::read_to_string(&record).expect("the reader said where it wrote");
        assert!(
            !Path::new(written.trim()).exists(),
            "{written} is still there"
        );
    }

    #[test]
    fn a_fetch_arrives_through_drain_without_being_waited_on() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        wrote_png(dir.path(), "shot.png");
        let mut clipboard = Clipboard::with(fetcher(dir.path(), Vec::new()));

        clipboard.fetch(&Source::File("shot.png".to_owned()));

        let started = Instant::now();
        let mut fetched = Vec::new();
        while fetched.is_empty() {
            assert!(started.elapsed() < Duration::from_secs(10), "it never came");
            fetched = clipboard.drain();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            matches!(fetched.as_slice(), [Fetched { source: Source::File(path), image: Ok(_) }] if path == "shot.png"),
            "{fetched:?}"
        );
    }
}
