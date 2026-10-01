// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! An image the operator attached to a prompt.
//!
//! The shell takes one from the clipboard or from a file and hands it to the
//! backend with the turn it belongs to. It is kept here, rather than in the
//! shell or a bridge, because both have to name it: the shell attaches it, a
//! bridge writes it on its wire, and neither may depend on the other.
//!
//! What an image is is read from its bytes, never from a file name: a
//! screenshot saved as `.png` by a tool that wrote a JPEG is still a JPEG, and
//! a backend told the wrong type refuses the turn.

/// The kinds of image a model accepts in a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaType {
    /// `image/png`
    Png,
    /// `image/jpeg`
    Jpeg,
    /// `image/gif`
    Gif,
    /// `image/webp`
    Webp,
}

impl MediaType {
    /// The kind of image `bytes` hold, read from their signature, or `None`
    /// where they are none of the kinds a model accepts.
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if bytes.starts_with(b"\xff\xd8\xff") {
            Some(Self::Jpeg)
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some(Self::Gif)
        } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
            Some(Self::Webp)
        } else {
            None
        }
    }

    /// The type as a MIME string, which is how every wire names it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
        }
    }
}

impl std::fmt::Display for MediaType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The most an image may take once base64-encoded, which is how every wire a
/// model is reached over carries one: the Messages API refuses an image
/// larger than 5 MB, and a refused image fails the whole turn.
pub const MAX_ENCODED_BYTES: usize = 5 * 1024 * 1024;

/// Why bytes could not be attached as an image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageError {
    /// Nothing was there.
    Empty,
    /// The bytes are not a PNG, JPEG, GIF or WebP image.
    NotAnImage,
    /// The image is larger than a model takes, by its encoded size.
    TooLarge {
        /// Its size once base64-encoded.
        encoded: usize,
    },
}

impl std::fmt::Display for ImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("the image is empty"),
            Self::NotAnImage => f.write_str("it is not a PNG, JPEG, GIF or WebP image"),
            Self::TooLarge { encoded } => write!(
                f,
                "the image is {} encoded, and a model takes at most {}",
                megabytes(*encoded),
                megabytes(MAX_ENCODED_BYTES)
            ),
        }
    }
}

impl std::error::Error for ImageError {}

/// A size in megabytes to one decimal, as the operator reads it.
fn megabytes(bytes: usize) -> String {
    // Display only: a size here is a message, never a figure anything adds up.
    #[allow(clippy::cast_precision_loss)]
    let mb = bytes as f64 / (1024.0 * 1024.0);
    format!("{mb:.1} MB")
}

/// An image, as the bytes of its file and the kind they were read as.
///
/// Only [`Image::from_bytes`] makes one, so an image always holds bytes of the
/// kind it says, and never more than a model takes.
#[derive(Clone, PartialEq, Eq)]
pub struct Image {
    media_type: MediaType,
    data: Vec<u8>,
}

impl Image {
    /// The image `data` holds, read from its signature, if a model can take
    /// it.
    pub fn from_bytes(data: Vec<u8>) -> Result<Self, ImageError> {
        if data.is_empty() {
            return Err(ImageError::Empty);
        }
        let media_type = MediaType::sniff(&data).ok_or(ImageError::NotAnImage)?;
        let encoded = encoded_len(data.len());
        if encoded > MAX_ENCODED_BYTES {
            return Err(ImageError::TooLarge { encoded });
        }
        Ok(Self { media_type, data })
    }

    /// What kind of image it is.
    pub fn media_type(&self) -> MediaType {
        self.media_type
    }

    /// The bytes of its file.
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

// By hand, because the bytes would fill a screen: what an image is and how
// big is all a person reading a debug print needs.
impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Image")
            .field("media_type", &self.media_type)
            .field("bytes", &self.data.len())
            .finish()
    }
}

/// How long `len` bytes are once base64-encoded with padding.
pub fn encoded_len(len: usize) -> usize {
    len.div_ceil(3).saturating_mul(4)
}

/// The placeholder a prompt holds where image `number` was attached, as
/// Claude Code writes it, so a prompt reads the same in either.
pub fn placeholder(number: u32) -> String {
    format!("[Image #{number}]")
}

/// The numbers of the image placeholders in `text`, in the order they appear.
pub fn placeholders_in(text: &str) -> Vec<u32> {
    const OPEN: &str = "[Image #";
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(OPEN) {
        rest = &rest[at + OPEN.len()..];
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits > 0
            && rest[digits..].starts_with(']')
            && let Ok(number) = rest[..digits].parse()
        {
            found.push(number);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    #[test]
    fn each_kind_is_read_from_its_signature() {
        assert_eq!(MediaType::sniff(PNG), Some(MediaType::Png));
        assert_eq!(
            MediaType::sniff(b"\xff\xd8\xff\xe0\0\x10JFIF"),
            Some(MediaType::Jpeg)
        );
        assert_eq!(MediaType::sniff(b"GIF89a\x01\0"), Some(MediaType::Gif));
        assert_eq!(MediaType::sniff(b"GIF87a\x01\0"), Some(MediaType::Gif));
        assert_eq!(
            MediaType::sniff(b"RIFF\x24\0\0\0WEBPVP8 "),
            Some(MediaType::Webp)
        );
    }

    #[test]
    fn a_riff_file_that_is_not_webp_is_not_an_image() {
        assert_eq!(MediaType::sniff(b"RIFF\x24\0\0\0WAVEfmt "), None);
    }

    #[test]
    fn text_saved_under_an_image_name_is_refused() {
        assert_eq!(
            Image::from_bytes(b"not an image".to_vec()),
            Err(ImageError::NotAnImage)
        );
        assert_eq!(Image::from_bytes(Vec::new()), Err(ImageError::Empty));
    }

    #[test]
    fn an_image_keeps_its_bytes_and_the_kind_they_were_read_as() {
        let image = Image::from_bytes(PNG.to_vec()).expect("a PNG signature is a PNG");
        assert_eq!(image.media_type(), MediaType::Png);
        assert_eq!(image.media_type().as_str(), "image/png");
        assert_eq!(image.data(), PNG);
    }

    #[test]
    fn an_image_is_refused_once_its_encoding_passes_the_limit_and_not_before() {
        // 3 bytes encode as 4, so this many raw bytes encode to the limit exactly.
        let at_limit = MAX_ENCODED_BYTES / 4 * 3;
        let mut data = PNG.to_vec();
        data.resize(at_limit, 0);
        assert!(Image::from_bytes(data.clone()).is_ok());

        data.push(0);
        assert_eq!(
            Image::from_bytes(data),
            Err(ImageError::TooLarge {
                encoded: MAX_ENCODED_BYTES + 4
            })
        );
    }

    #[test]
    fn the_encoded_length_is_padded_to_whole_quads() {
        assert_eq!(encoded_len(0), 0);
        assert_eq!(encoded_len(1), 4);
        assert_eq!(encoded_len(3), 4);
        assert_eq!(encoded_len(4), 8);
    }

    #[test]
    fn a_refusal_for_size_names_both_sizes() {
        let said = ImageError::TooLarge {
            encoded: 7 * 1024 * 1024,
        }
        .to_string();
        assert_eq!(
            said,
            "the image is 7.0 MB encoded, and a model takes at most 5.0 MB"
        );
    }

    #[test]
    fn placeholders_are_found_in_order_and_lookalikes_are_not() {
        let text = format!(
            "see {} and {}, not [Image #] or [Image #x] or [Image #4",
            placeholder(3),
            placeholder(12)
        );
        assert_eq!(placeholders_in(&text), vec![3, 12]);
    }
}
