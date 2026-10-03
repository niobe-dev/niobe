// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Where an image the operator attaches to a prompt comes from.
//!
//! The shell reads no clipboard and no file, so it says what it wants — the
//! image on the clipboard, or the one in a file whose path was pasted — and
//! the binary hands it something that fetches it, the way [`crate::shell`] is
//! handed something that runs commands.
//!
//! # How an image is attached
//!
//! * [`PASTE_KEY`] in the composer asks for the image on the clipboard. It is
//!   a key of its own because a terminal's paste hands over text: an image on
//!   the clipboard reaches the shell as nothing at all, or not at all.
//! * A paste that is nothing asks for it too, since a terminal that pastes an
//!   image it cannot give as text pastes an empty string.
//! * A paste that is one path to an image file — which is also what dropping
//!   a file on most terminals types — attaches that file instead of its path.
//!   A path that is not attached goes into the prompt as it was pasted.
//!
//! What is attached is written into the prompt as `[Image #N]`, where it went,
//! numbered across the session as Claude Code numbers them, and sent with the
//! turn. Deleting the placeholder before sending drops the image.
//!
//! A fetch is never waited on: the clipboard is read by another program, and
//! the loop has a terminal to draw on every tick. Its outcome arrives through
//! [`Images::drain`], and until it has, the prompt it is for is not sent.

use std::collections::VecDeque;

use niobe_core::image::{Image, placeholder, placeholders_in};

/// The key that attaches the image on the clipboard, as the help names it.
pub const PASTE_KEY: &str = "Ctrl+V";

/// Where an image is to be fetched from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The image on the clipboard.
    Clipboard,
    /// The file at a path the operator pasted, as it was pasted once its
    /// quoting, escapes or `file://` address were taken off: a relative path
    /// is the session's directory's, and `~` is the operator's home.
    File(String),
}

/// How one fetch ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    /// What was asked for.
    pub source: Source,
    /// The image, or why there is none, in words the operator reads.
    pub image: Result<Image, String>,
}

/// Something that fetches images for the shell.
pub trait Images: std::fmt::Debug {
    /// Starts fetching from `source`. Returns at once; the outcome, a failure
    /// to start included, arrives through [`Images::drain`].
    fn fetch(&mut self, source: &Source);

    /// Every fetch that has ended since the last call, in the order they
    /// ended. Never blocks.
    fn drain(&mut self) -> Vec<Fetched>;
}

/// Nothing to fetch from: a recorded log being looked at rather than
/// continued. Every fetch ends at once, saying so.
#[derive(Debug, Clone, Default)]
pub struct NoImages {
    asked: Vec<Source>,
}

impl Images for NoImages {
    fn fetch(&mut self, source: &Source) {
        self.asked.push(source.clone());
    }

    fn drain(&mut self) -> Vec<Fetched> {
        std::mem::take(&mut self.asked)
            .into_iter()
            .map(|source| Fetched {
                source,
                image: Err("this session cannot attach images".to_owned()),
            })
            .collect()
    }
}

/// The images a session has attached and not yet sent, and what it has asked
/// for and not yet been handed.
///
/// An image is numbered as it is attached and sent with the next prompt that
/// still holds its placeholder. The numbers run on across the session, and
/// past any the session already holds when it is read back, so no two images
/// a transcript shows share one.
#[derive(Debug, Default)]
pub(crate) struct Attachments {
    /// The number the last image attached, or read back, was given.
    last: u32,
    /// Attached to the prompt being written, oldest first.
    waiting: Vec<(u32, Image)>,
    /// Asked for since the event loop last looked.
    asked: Vec<Source>,
    /// Asked for and not yet handed back, so the bar can say one is coming.
    fetching: usize,
    /// Each pasted path asked for and not yet handed back, with the paste it
    /// was read from, oldest first.
    pasted: Vec<(String, String)>,
    /// The images of each prompt sent and not yet handed to the backend,
    /// oldest first.
    turns: VecDeque<Vec<Image>>,
}

impl Attachments {
    /// Asks for an image from `source`.
    pub(crate) fn ask(&mut self, source: Source) {
        self.asked.push(source);
        self.fetching = self.fetching.saturating_add(1);
    }

    /// Notes that the file at `path` was asked for because `pasted` named
    /// it, so that a file which is not attached gives the paste back.
    pub(crate) fn remember_paste(&mut self, path: String, pasted: String) {
        self.pasted.push((path, pasted));
    }

    /// What was asked for since the last call, oldest first.
    pub(crate) fn take_asked(&mut self) -> Vec<Source> {
        std::mem::take(&mut self.asked)
    }

    /// Whether an image asked for has not come back yet.
    pub(crate) fn fetching(&self) -> bool {
        self.fetching > 0
    }

    /// Notes that the fetch from `source` ended, however it ended, and
    /// returns the paste that asked for it, where a paste did.
    pub(crate) fn fetched(&mut self, source: &Source) -> Option<String> {
        self.fetching = self.fetching.saturating_sub(1);
        let Source::File(path) = source else {
            return None;
        };
        let at = self.pasted.iter().position(|(asked, _)| asked == path)?;
        Some(self.pasted.remove(at).1)
    }

    /// Attaches `image` to the prompt being written, and returns the
    /// placeholder that stands for it there.
    pub(crate) fn attach(&mut self, image: Image) -> String {
        self.last = self.last.saturating_add(1);
        self.waiting.push((self.last, image));
        placeholder(self.last)
    }

    /// Numbers the next image past every placeholder in `text`, a prompt this
    /// session already holds.
    pub(crate) fn seen(&mut self, text: &str) {
        if let Some(&highest) = placeholders_in(text).iter().max() {
            self.last = self.last.max(highest);
        }
    }

    /// Sends the prompt `text`: the images whose placeholders it still holds
    /// go with it, in the order they were attached, and the rest are dropped.
    pub(crate) fn send(&mut self, text: &str) {
        let named = placeholders_in(text);
        let images = std::mem::take(&mut self.waiting)
            .into_iter()
            .filter(|(number, _)| named.contains(number))
            .map(|(_, image)| image)
            .collect();
        self.turns.push_back(images);
    }

    /// Notes a turn sent with no images, so the ones attached to the prompt
    /// still being written stay with it and later turns keep theirs.
    pub(crate) fn send_none(&mut self) {
        self.turns.push_back(Vec::new());
    }

    /// Drops what is attached to a prompt that will not be sent.
    pub(crate) fn discard(&mut self) {
        self.waiting.clear();
    }

    /// The images of the oldest prompt sent and not yet handed on.
    pub(crate) fn take_turn(&mut self) -> Vec<Image> {
        self.turns.pop_front().unwrap_or_default()
    }
}

/// The extensions a pasted path is taken as an image by, as Claude Code
/// takes them. What the file holds is still read from its bytes.
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

/// The path in `pasted`, where the paste is one path to an image file and
/// nothing else.
///
/// A dropped file arrives quoted, or with its spaces escaped, or as a
/// `file://` address, depending on the terminal; each is taken back to the
/// path. Text with a space no backslash escapes is a sentence, not a path,
/// however it ends, unless it is quoted whole; and anything with a line break
/// in it is text.
pub fn image_path(pasted: &str) -> Option<String> {
    let trimmed = pasted.trim();
    if trimmed.is_empty() || trimmed.contains(['\n', '\r']) {
        return None;
    }
    let unquoted = match quoted(trimmed) {
        Some(inner) => inner.to_owned(),
        None if trimmed.starts_with(FILE_SCHEME) => trimmed.to_owned(),
        None => unescaped(trimmed)?,
    };
    let path = match unquoted.strip_prefix(FILE_SCHEME) {
        Some(address) => file_address_path(address)?,
        None => unquoted,
    };
    names_an_image(&path).then_some(path)
}

/// Whether `path`, a path as it is written on disk, names an image file by
/// its extension.
pub fn names_an_image(path: &str) -> bool {
    let Some((_, extension)) = path.rsplit_once('.') else {
        return false;
    };
    // A name alone, with nothing before its extension, is not a file anyone
    // dropped.
    let name = path.rsplit('/').next().unwrap_or(path);
    IMAGE_EXTENSIONS
        .iter()
        .any(|known| extension.eq_ignore_ascii_case(known))
        && name.len() > extension.len() + 1
}

/// How a dropped file's address starts.
const FILE_SCHEME: &str = "file://";

/// `text` without the quotes around it, where it is quoted whole.
fn quoted(text: &str) -> Option<&str> {
    ['\'', '"'].into_iter().find_map(|quote| {
        text.strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
    })
}

/// `text` with its backslash escapes taken out, or `None` where it holds
/// whitespace no backslash escapes, which a terminal never leaves in a path
/// it types.
fn unescaped(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.extend(chars.next()),
            c if c.is_whitespace() => return None,
            c => out.push(c),
        }
    }
    Some(out)
}

/// The local path a `file://` address names, given what follows the scheme:
/// its host empty or `localhost`, and its percent escapes decoded, since a
/// terminal escapes a space or a byte past ASCII in a name it drops this way.
/// `None` where the address names another host or decodes to no text.
fn file_address_path(address: &str) -> Option<String> {
    let path = address.strip_prefix("localhost").unwrap_or(address);
    if !path.starts_with('/') {
        return None;
    }
    String::from_utf8(percent_decoded(path)).ok()
}

/// The bytes of `text` with each `%` and two hex digits taken as the byte
/// they name; a `%` without them stays as it is.
fn percent_decoded(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        let escaped = (byte == b'%')
            .then(|| bytes.get(at + 1..at + 3))
            .flatten()
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match escaped {
            Some(decoded) => {
                out.push(decoded);
                at += 3;
            }
            None => {
                out.push(byte);
                at += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pasted_image_path_is_taken_as_a_path_whatever_the_terminal_wrapped_it_in() {
        for pasted in [
            "/Users/me/Desktop/shot.png",
            "  /Users/me/Desktop/shot.png\n",
            "'/Users/me/Desktop/shot.png'",
            "\"/Users/me/Desktop/shot.png\"",
            "file:///Users/me/Desktop/shot.png",
        ] {
            assert_eq!(
                image_path(pasted).as_deref(),
                Some("/Users/me/Desktop/shot.png"),
                "{pasted:?}"
            );
        }
        assert_eq!(
            image_path(r"/Users/me/Screen\ Shot\ 1.JPG").as_deref(),
            Some("/Users/me/Screen Shot 1.JPG")
        );
        assert_eq!(image_path("shots/a.webp").as_deref(), Some("shots/a.webp"));
    }

    #[test]
    fn text_that_is_not_one_image_path_is_text() {
        for pasted in [
            "",
            "   ",
            "notes.txt",
            "see /tmp/a.png\nand /tmp/b.png",
            ".png",
            "/tmp/.png",
            "png",
        ] {
            assert_eq!(image_path(pasted), None, "{pasted:?}");
        }
    }

    #[test]
    fn a_sentence_that_ends_in_an_image_name_is_text() {
        for pasted in [
            "rename the logo to logo.png",
            r"the regex is ^shot\d+\.png",
            "/Users/me/Screen Shot.png",
        ] {
            assert_eq!(image_path(pasted), None, "{pasted:?}");
        }
        assert_eq!(
            image_path("'rename the logo to logo.png'").as_deref(),
            Some("rename the logo to logo.png")
        );
    }

    #[test]
    fn a_dropped_file_address_is_read_back_to_the_path_it_names() {
        for (pasted, path) in [
            ("file:///Users/me/My%20Shot.png", "/Users/me/My Shot.png"),
            ("file://localhost/Users/me/shot.png", "/Users/me/shot.png"),
            ("file:///Users/me/%C3%A9t%C3%A9.png", "/Users/me/été.png"),
            ("file:///Users/me/100%.png", "/Users/me/100%.png"),
            ("'file:///Users/me/a%27b.png'", "/Users/me/a'b.png"),
        ] {
            assert_eq!(image_path(pasted).as_deref(), Some(path), "{pasted:?}");
        }
    }

    #[test]
    fn a_file_address_that_names_no_local_path_is_text() {
        for pasted in [
            "file://server/share/shot.png",
            "file://shot.png",
            "file:///Users/me/%FF.png",
        ] {
            assert_eq!(image_path(pasted), None, "{pasted:?}");
        }
    }

    fn image(tag: &[u8]) -> Image {
        let mut data = b"\x89PNG\r\n\x1a\n".to_vec();
        data.extend_from_slice(tag);
        Image::from_bytes(data).expect("a PNG signature")
    }

    #[test]
    fn a_prompt_sends_the_images_it_still_names_in_the_order_attached() {
        let mut attachments = Attachments::default();
        let first = attachments.attach(image(b"1"));
        let second = attachments.attach(image(b"2"));
        let third = attachments.attach(image(b"3"));
        assert_eq!(
            (first.as_str(), second.as_str()),
            ("[Image #1]", "[Image #2]")
        );

        // The second was deleted from the prompt before it went.
        attachments.send(&format!("{third} after {first}"));

        assert_eq!(attachments.take_turn(), vec![image(b"1"), image(b"3")]);
        assert_eq!(attachments.take_turn(), Vec::new());
    }

    #[test]
    fn each_prompt_takes_its_own_images() {
        let mut attachments = Attachments::default();
        let first = attachments.attach(image(b"1"));
        attachments.send(&first);
        attachments.send("no image");
        let third = attachments.attach(image(b"3"));
        attachments.send(&third);

        assert_eq!(attachments.take_turn(), vec![image(b"1")]);
        assert_eq!(attachments.take_turn(), Vec::new());
        assert_eq!(attachments.take_turn(), vec![image(b"3")]);
        assert_eq!(third, "[Image #2]");
    }

    #[test]
    fn numbering_runs_on_past_the_images_a_session_already_holds() {
        let mut attachments = Attachments::default();
        attachments.seen("before: [Image #3] and [Image #1]");
        attachments.seen("nothing here");
        assert_eq!(attachments.attach(image(b"4")), "[Image #4]");
    }

    #[test]
    fn a_discarded_prompt_sends_nothing_it_had() {
        let mut attachments = Attachments::default();
        let first = attachments.attach(image(b"1"));
        attachments.discard();
        attachments.send(&first);
        assert_eq!(attachments.take_turn(), Vec::new());
    }

    #[test]
    fn a_fetch_is_outstanding_until_it_is_handed_back() {
        let mut attachments = Attachments::default();
        attachments.ask(Source::Clipboard);
        assert!(attachments.fetching());
        assert_eq!(attachments.take_asked(), vec![Source::Clipboard]);
        assert!(attachments.take_asked().is_empty());
        assert_eq!(attachments.fetched(&Source::Clipboard), None);
        assert!(!attachments.fetching());
    }

    #[test]
    fn a_pasted_path_hands_back_the_paste_it_was_read_from_once() {
        let mut attachments = Attachments::default();
        let source = Source::File("a b.png".to_owned());
        attachments.remember_paste("a b.png".to_owned(), r"a\ b.png".to_owned());
        attachments.ask(source.clone());
        attachments.ask(Source::File("c.png".to_owned()));

        assert_eq!(attachments.fetched(&source).as_deref(), Some(r"a\ b.png"));
        assert_eq!(attachments.fetched(&source), None);
    }

    #[test]
    fn a_session_with_nothing_to_fetch_from_says_so_for_every_fetch() {
        let mut images = NoImages::default();
        images.fetch(&Source::Clipboard);
        let fetched = images.drain();
        assert!(
            matches!(
                fetched.as_slice(),
                [Fetched {
                    source: Source::Clipboard,
                    image: Err(_)
                }]
            ),
            "{fetched:?}"
        );
        assert!(images.drain().is_empty());
    }
}
