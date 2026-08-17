//! The image formats a story attachment may hold, and how one is identified.
//!
//! # Sniffed, never trusted (SH-315)
//!
//! [`MediaType::sniff`] reads magic bytes and nothing else — never a
//! filename's extension, never a caller-declared content type. An
//! attachment's format is a fact about its bytes; a name is a claim someone
//! made about them, and `shot.png` containing HTML is refused rather than
//! silently accepted.
//!
//! # SVG is deliberately excluded
//!
//! The four variants here are fixed-pixel raster formats. SVG is
//! script-bearing markup — an `<image>` tag can carry an `onload` handler —
//! and once a byte-serving route exists (this epic's child B,
//! `docs/spec/story-attachments.md`) serving one back to a browser at a
//! same-origin URL would be a stored-XSS sink. Refusing it here, at the one
//! place every attachment's format is decided, is what keeps that true
//! regardless of what a future route does or does not sanitize.

use serde::{Deserialize, Serialize};

/// One of the four image formats storyhook accepts as a story attachment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaType {
    Png,
    Jpeg,
    Gif,
    Webp,
}

impl MediaType {
    /// The IANA media type this format serves as, over HTTP.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
        }
    }

    /// The canonical file extension for this format — `story attachment
    /// save`'s default filename when the caller does not choose one.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Gif => "gif",
            Self::Webp => "webp",
        }
    }

    /// Identifies `bytes`' format from its magic-byte signature, or `None` if
    /// it opens with none of the four this project accepts — including a
    /// truncated file too short to carry any signature at all.
    #[must_use]
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        const JPEG_SIGNATURE: [u8; 3] = [0xFF, 0xD8, 0xFF];

        if bytes.starts_with(&PNG_SIGNATURE) {
            return Some(Self::Png);
        }
        if bytes.starts_with(&JPEG_SIGNATURE) {
            return Some(Self::Jpeg);
        }
        if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            return Some(Self::Gif);
        }
        // RIFF is a container format shared with WAV/AVI; the four bytes at
        // offset 8 name which one this file is.
        if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            return Some(Self::Webp);
        }
        None
    }
}

impl std::fmt::Display for MediaType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::MediaType;

    #[test]
    fn sniffs_a_real_png() {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        bytes.extend_from_slice(b"rest of the file does not matter");
        assert_eq!(MediaType::sniff(&bytes), Some(MediaType::Png));
    }

    #[test]
    fn sniffs_a_real_jpeg() {
        let bytes = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        assert_eq!(MediaType::sniff(&bytes), Some(MediaType::Jpeg));
    }

    #[test]
    fn sniffs_both_gif_signatures() {
        assert_eq!(MediaType::sniff(b"GIF87a..."), Some(MediaType::Gif));
        assert_eq!(MediaType::sniff(b"GIF89a..."), Some(MediaType::Gif));
    }

    #[test]
    fn sniffs_a_real_webp() {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&[0, 0, 0, 0]); // chunk size, irrelevant here
        bytes.extend_from_slice(b"WEBP");
        assert_eq!(MediaType::sniff(&bytes), Some(MediaType::Webp));
    }

    #[test]
    fn refuses_an_svg() {
        assert_eq!(
            MediaType::sniff(b"<svg xmlns='http://www.w3.org/2000/svg'></svg>"),
            None
        );
    }

    #[test]
    fn refuses_html_wearing_a_png_extension() {
        // The whole point of sniffing: the caller may have named this
        // `shot.png`, but its bytes say otherwise.
        assert_eq!(
            MediaType::sniff(b"<html><body>not an image</body></html>"),
            None
        );
    }

    #[test]
    fn refuses_an_empty_file() {
        assert_eq!(MediaType::sniff(&[]), None);
    }

    #[test]
    fn refuses_bytes_too_short_to_carry_any_signature() {
        assert_eq!(MediaType::sniff(&[0x89, b'P']), None);
    }

    #[test]
    fn refuses_a_riff_file_that_is_not_webp() {
        // RIFF is a shared container (WAV, AVI, WebP); only the fourth field
        // says which. A RIFF/WAVE file must not be mistaken for an image.
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(b"WAVE");
        assert_eq!(MediaType::sniff(&bytes), None);
    }

    #[test]
    fn every_variant_round_trips_through_its_own_serde_tag() {
        for (variant, tag) in [
            (MediaType::Png, "\"png\""),
            (MediaType::Jpeg, "\"jpeg\""),
            (MediaType::Gif, "\"gif\""),
            (MediaType::Webp, "\"webp\""),
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, tag);
            assert_eq!(serde_json::from_str::<MediaType>(&json).unwrap(), variant);
        }
    }

    #[test]
    fn every_variant_has_a_distinct_extension_and_media_type() {
        let variants = [
            MediaType::Png,
            MediaType::Jpeg,
            MediaType::Gif,
            MediaType::Webp,
        ];
        let extensions: std::collections::BTreeSet<_> =
            variants.iter().map(|v| v.extension()).collect();
        let media_types: std::collections::BTreeSet<_> =
            variants.iter().map(|v| v.as_str()).collect();
        assert_eq!(extensions.len(), variants.len());
        assert_eq!(media_types.len(), variants.len());
    }
}
