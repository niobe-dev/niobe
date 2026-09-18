// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Which config files may put an environment in front of a backend.
//!
//! A repository's config arrives with the clone. It can define profiles, and a
//! profile's `env`, `args` and `auth_refresh` are what a backend is started
//! with: a file nobody read could set `ANTHROPIC_BASE_URL`, and the official
//! CLI would then send the subscription it is signed in as to a host the
//! repository chose. Niobe would have touched no token file and still have
//! broken the line it stands on. `auth_refresh` is a shell command, and `args`
//! reach the same binary `env` does — the `claude` CLI takes an environment on
//! its own command line — so all three are the same exposure.
//!
//! So they take effect only once the operator has said, of the contents they
//! read, that this file may do that. The decision is recorded against the
//! file's SHA-256, under the user's own config directory: a record kept beside
//! the repository would be as clonable as the config, and one kept per path
//! would carry a decision made about bytes that are no longer there. Editing
//! the file — a pull, a rebase, a rewrite — asks again.
//!
//! The user's own config is not gated. It is the file the operator writes;
//! asking them to trust their own text would teach them to say yes without
//! reading, which is how a trust prompt stops being one.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::ConfigError;

/// The file name of the trust record, in the user's config directory.
///
/// Not TOML: a line is a digest and then a path to the end of it, so a path
/// with a quote, a backslash or a bracket in it needs no escaping and cannot
/// be read back as something else. Niobe writes this file; the operator reads
/// it, and deletes lines from it if they want to.
pub const FILE_NAME: &str = "trusted.list";

/// What is written above the entries, so that a file found in a config
/// directory says what it is.
const HEADER: &str = "\
# Written by niobe. Each line is a repository config this machine has been told
# it may start a backend with — its profiles' env, args and auth_refresh — and
# the SHA-256 of that file's contents when it was told so. Editing the config
# makes it untrusted again. `niobe untrust` removes a line; so does deleting it.
";

/// The SHA-256 of a config file's contents, as lower-case hex.
///
/// Of the text rather than of the parsed config: what the operator reads is
/// the file, comments and all, and a change that the parser happens to ignore
/// is still a change they have not seen.
pub fn digest(text: &str) -> String {
    let mut hex = String::with_capacity(64);
    for byte in Sha256::digest(text.as_bytes()) {
        // Writing into a String cannot fail, and the value is discarded rather
        // than unwrapped: there is no error here to report.
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// The config files this machine may start a backend with, and the contents
/// each was trusted at.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Trusted {
    entries: BTreeMap<PathBuf, String>,
}

impl Trusted {
    /// Reads the record at `path`. A file that is not there trusts nothing,
    /// which is not an error: it is every machine before the first `niobe
    /// trust`.
    pub fn read(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text, path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(ConfigError::Read {
                path: path.to_path_buf(),
                error,
            }),
        }
    }

    /// Parses the text of the record at `path`.
    ///
    /// A line that is not an entry stops the read rather than being skipped: a
    /// record half of which was understood would silently ungate whatever the
    /// unread half held back, and this is the file that says what may run.
    pub fn parse(text: &str, path: &Path) -> Result<Self, ConfigError> {
        let mut entries = BTreeMap::new();
        for (index, line) in text.lines().enumerate() {
            // Not trimmed: a path may end in a space, and this file is where
            // it has to come back as the one that was trusted.
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let invalid = |message: &str| ConfigError::Invalid {
                path: path.to_path_buf(),
                line: index + 1,
                key: None,
                message: message.to_owned(),
            };
            let (hex, file) = line
                .split_once(' ')
                .ok_or_else(|| invalid("expected a digest, a space and a path"))?;
            if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(invalid("expected a 64-character hexadecimal digest"));
            }
            if file.is_empty() {
                return Err(invalid("expected a path after the digest"));
            }
            entries.insert(PathBuf::from(file), hex.to_ascii_lowercase());
        }
        Ok(Self { entries })
    }

    /// Whether `file`, holding `text`, may start a backend with what it says.
    ///
    /// False for a file that was trusted at other contents: the decision was
    /// about bytes, not about a path.
    pub fn trusts(&self, file: &Path, text: &str) -> bool {
        self.entries.get(file) == Some(&digest(text))
    }

    /// Records `file`, holding `text`, as one the operator has read.
    ///
    /// A path with a newline in it cannot be written down, because a line of
    /// the record is one entry; it is refused rather than written as two
    /// entries that mean nothing.
    pub fn trust(&mut self, file: &Path, text: &str) -> Result<(), ConfigError> {
        let name = file.to_string_lossy();
        if name.contains('\n') || name.contains('\r') {
            return Err(ConfigError::Untrustable {
                path: file.to_path_buf(),
                message: "its path has a line break in it, and one entry is one line".to_owned(),
            });
        }
        self.entries.insert(file.to_path_buf(), digest(text));
        Ok(())
    }

    /// Forgets `file`, and says whether it had been trusted.
    pub fn forget(&mut self, file: &Path) -> bool {
        self.entries.remove(file).is_some()
    }

    /// The record as it is written down.
    pub fn render(&self) -> String {
        let mut text = String::from(HEADER);
        for (file, hex) in &self.entries {
            let _ = writeln!(text, "{hex} {}", file.display());
        }
        text
    }

    /// Writes the record to `path`, creating its directory if this is the
    /// first file trusted on this machine.
    pub fn write(&self, path: &Path) -> Result<(), ConfigError> {
        let failed = |error: std::io::Error| ConfigError::Read {
            path: path.to_path_buf(),
            error,
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(failed)?;
        }
        std::fs::write(path, self.render()).map_err(failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file() -> PathBuf {
        PathBuf::from("/repo/.niobe/config.toml")
    }

    #[test]
    fn a_digest_is_the_published_sha_256_of_the_files_text() {
        // The two vectors from FIPS 180-4's own examples, so that a change of
        // hash function fails here rather than silently re-trusting nothing.
        assert_eq!(
            digest(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            digest("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_file_is_trusted_at_the_contents_it_was_trusted_at_and_at_no_others() {
        let mut trusted = Trusted::default();
        trusted
            .trust(&file(), "backend = \"claude\"\n")
            .expect("a plain path");

        assert!(trusted.trusts(&file(), "backend = \"claude\"\n"));
        assert!(!trusted.trusts(&file(), "backend = \"codex\"\n"));
        assert!(!trusted.trusts(
            Path::new("/other/.niobe/config.toml"),
            "backend = \"claude\"\n"
        ));
        assert!(!Trusted::default().trusts(&file(), "backend = \"claude\"\n"));
    }

    #[test]
    fn a_comment_that_the_parser_ignores_is_still_a_change_to_the_file() {
        let mut trusted = Trusted::default();
        trusted
            .trust(&file(), "[profiles.p]\nbackend = \"claude\"\n")
            .expect("a plain path");

        assert!(!trusted.trusts(&file(), "# read me\n[profiles.p]\nbackend = \"claude\"\n"));
    }

    #[test]
    fn what_was_written_reads_back_and_a_forgotten_file_is_gone() {
        let mut trusted = Trusted::default();
        trusted.trust(&file(), "a").expect("a plain path");
        trusted
            .trust(Path::new("/other \"quoted\"/.niobe/config.toml"), "b")
            .expect("a path no escaping would survive");

        let text = trusted.render();
        assert_eq!(
            Trusted::parse(&text, Path::new("/u/trusted.list")).expect("valid"),
            trusted
        );

        assert!(trusted.forget(&file()));
        assert!(!trusted.forget(&file()));
        let text = trusted.render();
        let read = Trusted::parse(&text, Path::new("/u/trusted.list")).expect("valid");
        assert!(!read.trusts(&file(), "a"));
        assert!(read.trusts(Path::new("/other \"quoted\"/.niobe/config.toml"), "b"));
    }

    #[test]
    fn a_record_that_is_not_one_is_reported_at_its_line() {
        let store = PathBuf::from("/u/trusted.list");
        let said = |text: &str| {
            Trusted::parse(text, &store)
                .expect_err("the line is not an entry")
                .to_string()
        };

        assert_eq!(
            said(&format!("{HEADER}{} /a\nnodigest\n", digest("a"))),
            "/u/trusted.list:6: expected a digest, a space and a path"
        );
        assert_eq!(
            said("beef /a\n"),
            "/u/trusted.list:1: expected a 64-character hexadecimal digest"
        );
        assert_eq!(
            said(&format!("{} \n", digest("a"))),
            "/u/trusted.list:1: expected a path after the digest"
        );
    }

    #[test]
    fn a_record_that_is_not_there_trusts_nothing() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let missing = dir.path().join(FILE_NAME);

        let trusted = Trusted::read(&missing).expect("absence is fine");

        assert_eq!(trusted, Trusted::default());
        assert!(!missing.exists(), "reading it created nothing");
    }

    #[test]
    fn writing_the_record_makes_the_config_directory_it_belongs_in() {
        let dir = tempfile::tempdir().expect("a temporary directory can be created");
        let path = dir.path().join("niobe").join(FILE_NAME);
        let mut trusted = Trusted::default();
        trusted.trust(&file(), "a").expect("a plain path");

        trusted.write(&path).expect("the record is written");

        assert_eq!(Trusted::read(&path).expect("it reads back"), trusted);
        assert!(
            std::fs::read_to_string(&path)
                .expect("the record reads")
                .starts_with("# Written by niobe."),
            "a file in a config directory says what it is"
        );
    }

    #[test]
    fn a_path_a_line_cannot_hold_is_refused_rather_than_written_as_two() {
        let said = Trusted::default()
            .trust(Path::new("/re\npo/.niobe/config.toml"), "a")
            .expect_err("one entry is one line")
            .to_string();

        assert!(said.starts_with("cannot record trust for "), "{said}");
        assert!(said.contains("line break"), "{said}");
    }
}
