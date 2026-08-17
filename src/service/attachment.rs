//! `story attachment add|list|remove|save` — image attachments on a story
//! (SH-315, the storage-and-CLI foundation child of the SH-315 epic).
//!
//! See [`docs/spec/story-attachments.md`](../../../docs/spec/story-attachments.md)
//! for the design of record: why an attachment's identity and metadata are
//! events while its bytes are a row in `story_attachment_blobs`, why the
//! format is sniffed from magic bytes rather than trusted from a filename,
//! and why SVG is refused.

use sha2::{Digest, Sha256};

use crate::domain::media_type::MediaType;
use crate::domain::{Attachment, StoryEvent, StorySnapshot};
use crate::error::AppError;
use crate::store::{ExpectedSeq, ReadOps, Store, StoreError, WriteOps};

use super::{Ctx, append_and_fold, project_prefix, resolve_open_story, resolve_story};

/// The largest attachment `add` accepts.
///
/// 10 MiB — generous for a screenshot or a diagram, and small enough that one
/// misattached file cannot dominate a store's daily backup. Enforced only on
/// the ordinary write path, in [`AttachmentService::add`]: like the label
/// guard in [`crate::domain::validate_event_for_append`], a replay
/// (`story import-project`) carries whatever a legitimately-exported document
/// already held, even if that binary's own cap was once different.
pub const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;

/// Attachment management over one story in one project.
pub struct AttachmentService<'ctx, S: Store> {
    ctx: &'ctx Ctx<'ctx, S>,
}

impl<'ctx, S: Store> AttachmentService<'ctx, S> {
    /// An attachment service bound to `ctx`.
    pub fn new(ctx: &'ctx Ctx<'ctx, S>) -> Self {
        Self { ctx }
    }

    /// Attaches `bytes` — already read off disk by the caller, since a
    /// relative path means the invoking shell's directory, which only the
    /// caller (CLI or REST handler) knows how to resolve against the
    /// request's own `cwd` — to an open story.
    ///
    /// Refuses a closed story (`Intent::Edit`, not `Intent::Append` — see
    /// [`super::Intent`]'s own doc comment on the pinned set of appends to a
    /// closed story, which this does not join): an attachment is part of what
    /// a story *is*, the same as its description, not an observation
    /// recorded about it after the fact.
    ///
    /// Refuses anything [`MediaType::sniff`] does not recognise — including
    /// an SVG, deliberately, see that function's own doc comment — and
    /// anything over [`MAX_ATTACHMENT_BYTES`].
    ///
    /// `name` defaults to the source path's file name, or `attachment` if the
    /// path carries none (a bare `attachment add SH-1 -` is not a shape the
    /// CLI produces today, but a REST caller is free to hand this function
    /// any string).
    pub fn add(
        &self,
        id: &str,
        bytes: &[u8],
        source_name: &str,
        name: Option<&str>,
    ) -> Result<StorySnapshot, AppError> {
        if bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(AppError::Validation(format!(
                "attachment is {} bytes, over the {MAX_ATTACHMENT_BYTES}-byte limit",
                bytes.len()
            )));
        }
        let media_type = MediaType::sniff(bytes).ok_or_else(|| {
            AppError::Validation(
                "not a supported image — storyhook accepts PNG, JPEG, GIF or WebP, identified \
                 from the file's own bytes rather than its name"
                    .to_string(),
            )
        })?;
        let sha256 = format!("{:x}", Sha256::digest(bytes));
        let name = name.map_or_else(|| default_attachment_name(source_name), str::to_string);
        let byte_len = bytes.len() as u64;

        let now = self.ctx.now();
        let project = self.ctx.project();
        Ok(self.ctx.store().write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let states = tx.state_map(project)?;
            let (story_no, row) = resolve_open_story(&*tx, project, &prefix, id)?;
            // From the snapshot's own monotonic counter, never from
            // `max(current attachments) + 1` — see
            // `StorySnapshot::next_attachment_id`'s own doc comment for why
            // that computation reuses an id the moment its attachment is
            // removed.
            let attachment_id = row.snapshot.next_attachment_id;
            let snapshot = append_and_fold(
                tx,
                project,
                story_no,
                &prefix,
                &states,
                ExpectedSeq::Exact(row.head_seq),
                &[StoryEvent::StoryAttachmentAdded {
                    at: now.clone(),
                    id: attachment_id,
                    name: name.clone(),
                    media_type,
                    byte_len,
                    sha256: sha256.clone(),
                }],
                self.ctx.provenance(),
            )?;
            tx.put_attachment_blob(project, story_no, attachment_id, bytes, &sha256, &now)?;
            Ok(snapshot)
        })?)
    }

    /// Every attachment on a story, in the order they were added — read-only,
    /// so it works on a closed story exactly as `story show` does.
    pub fn list(&self, id: &str) -> Result<Vec<Attachment>, AppError> {
        let project = self.ctx.project();
        Ok(self.ctx.store().read(|tx| {
            let prefix = project_prefix(tx, project)?;
            let (_, row) = resolve_story(tx, project, &prefix, id)?;
            Ok(row.snapshot.attachments)
        })?)
    }

    /// Removes an attachment from an open story, deleting its bytes.
    ///
    /// Refuses a closed story for the same reason [`Self::add`] does. Not a
    /// tombstone: see [`StoryEvent::StoryAttachmentRemoved`]'s own doc
    /// comment for why the bytes are genuinely deleted rather than merely
    /// hidden.
    pub fn remove(&self, id: &str, attachment_id: u32) -> Result<StorySnapshot, AppError> {
        let now = self.ctx.now();
        let project = self.ctx.project();
        Ok(self.ctx.store().write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let states = tx.state_map(project)?;
            let (story_no, row) = resolve_open_story(&*tx, project, &prefix, id)?;
            if !row
                .snapshot
                .attachments
                .iter()
                .any(|a| a.id == attachment_id)
            {
                return Err(StoreError::NotFound(format!(
                    "story `{id}` has no attachment {attachment_id}"
                )));
            }
            let snapshot = append_and_fold(
                tx,
                project,
                story_no,
                &prefix,
                &states,
                ExpectedSeq::Exact(row.head_seq),
                &[StoryEvent::StoryAttachmentRemoved {
                    at: now.clone(),
                    id: attachment_id,
                }],
                self.ctx.provenance(),
            )?;
            tx.delete_attachment_blob(project, story_no, attachment_id)?;
            Ok(snapshot)
        })?)
    }

    /// The metadata and stored bytes of one attachment — read-only, working
    /// on a closed story exactly as [`Self::list`] does.
    ///
    /// A missing blob row behind a snapshot that names the attachment is
    /// reported as [`AppError::Integrity`] rather than
    /// [`AppError::NotFound`]: the attachment demonstrably exists (the
    /// snapshot says so), so the store itself is inconsistent, which is
    /// `story doctor`'s question rather than a caller's typo.
    pub fn get(&self, id: &str, attachment_id: u32) -> Result<(Attachment, Vec<u8>), AppError> {
        let project = self.ctx.project();
        Ok(self.ctx.store().read(|tx| {
            let prefix = project_prefix(tx, project)?;
            let (story_no, row) = resolve_story(tx, project, &prefix, id)?;
            let attachment = row
                .snapshot
                .attachments
                .into_iter()
                .find(|a| a.id == attachment_id)
                .ok_or_else(|| {
                    StoreError::NotFound(format!("story `{id}` has no attachment {attachment_id}"))
                })?;
            let bytes = tx
                .attachment_blob(project, story_no, attachment_id)?
                .ok_or_else(|| {
                    StoreError::Invariant(format!(
                        "story `{id}` attachment {attachment_id} has no stored bytes — run \
                         `story doctor`"
                    ))
                })?;
            Ok((attachment, bytes))
        })?)
    }
}

/// The name an attachment takes when the caller did not choose one — the
/// source path's own file name, or `attachment` when it has none (a path
/// ending in `..` or `/`, which `std::path::Path::file_name` already refuses
/// to invent one for).
fn default_attachment_name(source_name: &str) -> String {
    std::path::Path::new(source_name)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "attachment".to_string())
}
