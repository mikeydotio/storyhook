//! Single-image upload protocol. Bytes stay in memory until the attachment
//! service commits them atomically with their metadata.

use std::io::Read;

use crate::api::http::{Reply, error_reply, header_value, json_reply, text_reply};
use crate::daemon::http1::{Header, Request};
use crate::error::AppError;
use crate::output::render_response;
use crate::service::attachment::MAX_ATTACHMENT_BYTES;
use crate::service::{AttachmentService, Ctx};
use crate::store::Store;

/// A filename encoded with JavaScript's `encodeURIComponent`, not form encoding.
const NAME_HEADER: &str = "X-Storyhook-Attachment-Name";

/// Checks the upload's media type before any potentially large body is read.
/// The service still identifies the stored image from its magic bytes.
pub(crate) fn check_content_type(headers: &[Header]) -> Result<(), Reply> {
    let mut types = headers.iter().filter(|h| h.field.equiv("Content-Type"));
    let accepted = types.next().is_some_and(|header| {
        let media_type = header.value.split(';').next().unwrap_or("").trim();
        [
            "application/octet-stream",
            "image/png",
            "image/jpeg",
            "image/gif",
            "image/webp",
        ]
        .iter()
        .any(|allowed| media_type.eq_ignore_ascii_case(allowed))
    });
    if !accepted || types.next().is_some() {
        return Err(text_reply(
            415,
            "Content-Type must be application/octet-stream or a supported image type (PNG, JPEG, GIF, WebP)",
        ));
    }
    Ok(())
}

/// Acquires one bounded binary body through the transport's existing deadline
/// and framing decoder. Never drains rejected bodies or trusts declared length
/// as proof of the number of bytes actually received.
pub(crate) fn read(request: &mut Request) -> Result<Vec<u8>, Reply> {
    check_content_type(request.headers())?;
    if header_value(request.headers(), "Content-Length")
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|size| size > MAX_ATTACHMENT_BYTES as u64)
    {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    request
        .as_reader()
        .take(MAX_ATTACHMENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| text_reply(400, format!("failed to read attachment upload: {error}")))?;
    check_size(bytes.len())?;
    Ok(bytes)
}

fn too_large() -> Reply {
    text_reply(
        413,
        format!("attachment exceeds the {MAX_ATTACHMENT_BYTES}-byte limit"),
    )
}

fn check_size(len: usize) -> Result<(), Reply> {
    if len > MAX_ATTACHMENT_BYTES {
        Err(too_large())
    } else {
        Ok(())
    }
}

/// Decodes a filename strictly: URL component encoding leaves `+` literal,
/// and malformed escapes or invalid UTF-8 must never silently change a name.
fn source_name(headers: &[Header]) -> Result<String, AppError> {
    let invalid = || {
        AppError::Usage(format!(
            "{NAME_HEADER} must be one non-empty, percent-encoded UTF-8 filename without control characters"
        ))
    };
    let mut names = headers
        .iter()
        .filter(|header| header.field.equiv(NAME_HEADER));
    let Some(header) = names.next() else {
        return Ok("attachment".into());
    };
    if names.next().is_some() {
        return Err(invalid());
    }
    let raw = header.value.as_bytes();
    let mut decoded = Vec::with_capacity(raw.len());
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == b'%' {
            let hi = raw
                .get(index + 1)
                .and_then(|b| (*b as char).to_digit(16))
                .ok_or_else(invalid)?;
            let lo = raw
                .get(index + 2)
                .and_then(|b| (*b as char).to_digit(16))
                .ok_or_else(invalid)?;
            decoded.push((hi * 16 + lo) as u8);
            index += 3;
        } else {
            decoded.push(raw[index]);
            index += 1;
        }
    }
    let name = String::from_utf8(decoded).map_err(|_| invalid())?;
    if name.is_empty() || name.chars().any(char::is_control) {
        return Err(invalid());
    }
    Ok(name)
}

/// Attaches one image inside the REST router's resolved project context.
/// Like PATCH, this service door explicitly canonicalizes the story ID.
pub(crate) fn reply<S: Store>(
    ctx: &Ctx<'_, S>,
    id: &str,
    headers: &[Header],
    bytes: &[u8],
) -> Reply {
    if let Err(reply) = check_content_type(headers).and_then(|()| check_size(bytes.len())) {
        return reply;
    }
    (|| -> Result<Reply, AppError> {
        let name = source_name(headers)?;
        let id = crate::invoke::story_ids::canonicalize_one(ctx, id)?;
        AttachmentService::new(ctx).add(&id, bytes, &name, None)?;
        let response = ctx.story_view(&id)?;
        Ok(json_reply(201, render_response(&response, true, false)))
    })()
    .unwrap_or_else(|error| error_reply(&error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn a_filename_is_optional_and_decoded_once_without_form_semantics() {
        assert_eq!(source_name(&[]).unwrap(), "attachment");
        for (wire, expected) in [
            ("a+b.png", "a+b.png"),
            ("a%2520b.png", "a%20b.png"),
            ("%F0%9F%93%B7.png", "📷.png"),
        ] {
            assert_eq!(
                source_name(&[Header::from_bytes(NAME_HEADER, wire).unwrap()]).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn ambiguous_content_types_are_refused() {
        let header = Header::from_bytes("Content-Type", "image/png").unwrap();
        assert_eq!(
            check_content_type(&[header.clone(), header])
                .unwrap_err()
                .status,
            415
        );
    }

    proptest! {
        #[test]
        fn every_nonempty_control_free_unicode_filename_round_trips(
            name in ".{1,64}".prop_filter("filename has no control characters", |s| !s.chars().any(char::is_control))
        ) {
            let wire: String = name.as_bytes().iter().map(|b| format!("%{b:02X}")).collect();
            let headers = [Header::from_bytes(NAME_HEADER, wire).unwrap()];
            prop_assert_eq!(source_name(&headers).unwrap(), name);
        }
    }
}
