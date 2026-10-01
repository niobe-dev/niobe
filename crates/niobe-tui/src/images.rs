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
//!
//! What is attached is written into the prompt as `[Image #N]`, where it went,
//! numbered across the session as Claude Code numbers them, and sent with the
//! turn. Deleting the placeholder before sending drops the image.
//!
//! A fetch is never waited on: the clipboard is read by another program, and
//! the loop has a terminal to draw on every tick. Its outcome arrives through
//! [`Images::drain`].

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
    /// quoting was taken off: a relative path is the session's directory's,
    /// and `~` is the operator's home.
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

    /// What was asked for since the last call, oldest first.
    pub(crate) fn take_asked(&mut self) -> Vec<Source> {
        std::mem::take(&mut self.asked)
    }

    /// Whether an image asked for has not come back yet.
    pub(crate) fn fetching(&self) -> bool {
        self.fetching > 0
    }

    /// Notes that a fetch ended, however it ended.
    pub(crate) fn fetched(&mut self) {
        self.fetching = self.fetching.saturating_sub(1);
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
/// path. Anything with a line break in it is text, however it ends.
pub fn image_path(pasted: &str) -> Option<String> {
    let trimmed = pasted.trim();
    if trimmed.is_empty() || trimmed.contains(['\n', '\r']) {
        return None;
    }
    let unquoted = unquote(trimmed);
    let path = unquoted.strip_prefix("file://").unwrap_or(&unquoted);
    let (_, extension) = path.rsplit_once('.')?;
    if !IMAGE_EXTENSIONS
        .iter()
        .any(|known| extension.eq_ignore_ascii_case(known))
    {
        return None;
    }
    // A name alone, with nothing before its extension, is not a file anyone
    // dropped.
    let name = path.rsplit('/').next().unwrap_or(path);
    (name.len() > extension.len() + 1).then(|| path.to_owned())
}

/// `text` without the quotes around it, or with its backslash escapes taken
/// out where it has none.
fn unquote(text: &str) -> String {
    for quote in ['\'', '"'] {
        if let Some(inner) = text
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner.to_owned();
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.extend(chars.next()),
            c => out.push(c),
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
        attachments.fetched();
        assert!(!attachments.fetching());
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
