// Copyright 2026 Adobe. All rights reserved.
// This file is licensed to you under the Apache License,
// Version 2.0 (http://www.apache.org/licenses/LICENSE-2.0)
// or the MIT license (http://opensource.org/licenses/MIT),
// at your option.

// Unless required by applicable law or agreed to in writing,
// this software is distributed on an "AS IS" BASIS, WITHOUT
// WARRANTIES OR REPRESENTATIONS OF ANY KIND, either express or
// implied. See the LICENSE-MIT and LICENSE-APACHE files for the
// specific language governing permissions and limitations under
// each license.

//! ID3v2-tagged audio: MP3 and FLAC.
//!
//! Both formats carry C2PA (a `GEOB` frame) and XMP (a `PRIV` frame owned by
//! `XMP`) in a leading ID3v2 tag. For FLAC the tag precedes the `fLaC` marker,
//! which is a widely tolerated extension of the FLAC format, not part of it.
//! The audio is not interpreted: everything after the tag is one
//! [`MediaData`](SegmentKind::MediaData) segment, and only its first bytes
//! decide the media type (`fLaC` is `audio/flac`, anything else
//! `audio/mpeg`).
//!
//! # Structure
//!
//! The 10-byte tag header is a [`Preamble`](SegmentKind::Preamble) segment.
//! Each C2PA `GEOB` and XMP `PRIV` frame is one [`Metadata`](SegmentKind::Metadata)
//! segment: the frame header and its identifying prefix are the segment header,
//! the object data is the payload and the item. Consecutive other frames form
//! one `Other` segment, and padding (or an unparseable remainder of the tag) and
//! an ID3v2.4 footer are segments of their own. Parsing reads the frame headers
//! and a bounded prefix of candidate frames, never a frame body.
//!
//! # Policy
//!
//! - ID3v2.3 and v2.4 tags are read and written. A tag of another version, or
//!   with unsynchronisation, an extended header or unknown flags, is carried
//!   through as opaque bytes with no items and cannot take new metadata.
//! - A `GEOB` or `PRIV` frame that is compressed, encrypted, grouped,
//!   unsynchronised or carries a data length indicator is an ordinary frame.
//! - The `GEOB` text encoding may be any ID3 encoding. The MIME type must be
//!   `application/c2pa` or the deprecated `application/x-c2pa-manifest-store`.
//! - A write replaces the first item of a kind in place and drops further ones.
//!   Without one, the new frame goes after the last frame, before the padding.
//!   Unrelated frames are copied byte for byte. A write that changes nothing
//!   copies the file; otherwise the tag size is rewritten and a v2.4 footer is
//!   dropped.
//! - A C2PA data hash excludes the object data only; the frame header and the
//!   `GEOB` fields stay hashed.
//! - A frame that is malformed or runs past the tag ends the frames; the rest
//!   of the tag is carried through unparsed.

use std::sync::Arc;

use crate::{
    container_api::{
        BoxFuture, ContainerHandler, Existing, FormatSpec, Io, ItemInfo, MetadataSlot,
        PlanBuilder, StructureBuilder, WritePlan,
    },
    jumbf::superbox_type,
    ByteRange, C2paHash, Error, Item, ItemKind, Name, Result, SegmentId, SegmentKind, Structure,
    Updates,
};

/// The ID3 container's [`ContainerHandler::id`].
pub const ID: &str = "id3";

const MPEG: &str = "audio/mpeg";
const FLAC: &str = "audio/flac";
const FLAC_MARKER: &[u8; 4] = b"fLaC";

const GEOB: [u8; 4] = *b"GEOB";
const PRIV: [u8; 4] = *b"PRIV";
/// Text encoding byte (ISO-8859-1), MIME type, file name, description; each
/// string NUL terminated.
const GEOB_PREFIX: &[u8] = b"\0application/c2pa\0c2pa\0c2pa manifest store\0";
const PRIV_PREFIX: &[u8] = b"XMP\0";
const MIME: &[u8] = b"application/c2pa";
const MIME_DEPRECATED: &[u8] = b"application/x-c2pa-manifest-store";

/// Segment names, for the parts of a tag the writer treats specially.
const HEADER: Name = Name::FourCC(*b"ID3h");
const OPAQUE: Name = Name::FourCC(*b"ID3x");
const PADDING: Name = Name::FourCC(*b"PADD");
const FOOTER: Name = Name::FourCC(*b"3DI\0");

const HEADER_LEN: u64 = 10;
/// The largest size a synchsafe tag size can hold.
const MAX_TAG: u64 = 0x0fff_ffff;
/// How much of a frame body is read to decide whether it is C2PA or XMP.
const PEEK: usize = 256;

/// The ID3 container handler.
#[derive(Debug, Clone, Copy, Default)]
pub struct Id3Handler;

/// The shared ID3 handler.
pub fn handler() -> Arc<dyn ContainerHandler> {
    Arc::new(Id3Handler)
}

impl ContainerHandler for Id3Handler {
    fn id(&self) -> &'static str {
        ID
    }

    fn formats(&self) -> &[FormatSpec] {
        const F: &[FormatSpec] = &[
            FormatSpec::new(MPEG, "mp3").media_type_aliases(&["audio/mp3"]),
            FormatSpec::new(FLAC, "flac").media_type_aliases(&["audio/x-flac"]),
        ];
        F
    }

    /// An ID3 tag does not say what follows it, so this matches any tagged
    /// file; parsing tells MP3 from FLAC.
    fn detect(&self, header: &[u8]) -> bool {
        header.starts_with(b"ID3") || header.starts_with(FLAC_MARKER)
    }

    // No box structure to hash: the data hash excludes the object data.
    fn c2pa_hash(&self, _structure: &Structure) -> Option<C2paHash> {
        Some(C2paHash::Data)
    }

    fn c2pa_data_hash_exclusions(
        &self,
        _structure: &Structure,
        item: &Item,
    ) -> Result<Vec<ByteRange>> {
        Ok(item.extents().map(|e| e.range()).collect())
    }

    fn parse_async(self: Arc<Self>, io: Io) -> BoxFuture<'static, Result<Structure>> {
        Box::pin(parse_with(self, io))
    }

    fn write_plan_async<'f, 'a: 'f>(
        &'f self,
        src: &'f Structure,
        mut io: Io,
        updates: &'a Updates,
    ) -> BoxFuture<'f, Result<WritePlan<'a>>> {
        Box::pin(async move {
            // The tag's version and flags carry over to the rewritten header.
            let mut head = [0u8; 10];
            if src.segments().first().is_some_and(|s| s.name() == HEADER) {
                io.read_at(0, &mut head).await?;
            }
            plan(src, head, updates)
        })
    }

    fn compose(&self, kind: ItemKind, data: &[u8]) -> Result<Vec<u8>> {
        let (ty, prefix) = frame_type(kind).ok_or(Error::UnsupportedFormat)?;
        let len = u32::try_from(prefix.len() + data.len())
            .ok()
            .filter(|&n| u64::from(n) <= MAX_TAG)
            .ok_or(Error::LimitExceeded {
                what: "ID3 frame size",
                limit: MAX_TAG,
            })?;
        let mut out = Vec::with_capacity(10 + prefix.len() + data.len());
        out.extend_from_slice(&frame_header(4, ty, len));
        out.extend_from_slice(prefix);
        out.extend_from_slice(data);
        Ok(out)
    }
}

// ── ID3 syntax ───────────────────────────────────────────────────────────────

/// A synchsafe integer, or `None` if a byte has its high bit set.
fn synchsafe(bytes: &[u8]) -> Option<u32> {
    bytes.iter().try_fold(0u32, |n, &b| {
        (b & 0x80 == 0).then(|| (n << 7) | u32::from(b))
    })
}

fn to_synchsafe(n: u32) -> [u8; 4] {
    [
        (n >> 21) as u8 & 0x7f,
        (n >> 14) as u8 & 0x7f,
        (n >> 7) as u8 & 0x7f,
        n as u8 & 0x7f,
    ]
}

#[derive(Debug, Clone, Copy)]
struct Tag {
    major: u8,
    flags: u8,
    /// Bytes after the header, up to the footer.
    size: u32,
}

impl Tag {
    fn parse(h: &[u8; 10]) -> Result<Self> {
        let size = synchsafe(&h[6..10])
            .ok_or_else(|| Error::InvalidFormat("ID3 tag size is not synchsafe".into()))?;
        if !(2..=4).contains(&h[3]) {
            return Err(Error::UnsupportedFeature("ID3v2 version"));
        }
        Ok(Tag {
            major: h[3],
            flags: h[5],
            size,
        })
    }

    /// Frames can be walked in place: v2.3 or v2.4, without unsynchronisation, an
    /// extended header or flags this code does not know.
    fn readable(&self) -> bool {
        matches!(self.major, 3 | 4) && self.flags & !0x30 == 0 && (self.major == 4 || self.flags & 0x10 == 0)
    }

    fn footer(&self) -> u64 {
        if self.major == 4 && self.flags & 0x10 != 0 {
            HEADER_LEN
        } else {
            0
        }
    }

    /// Where the tag ends, footer included.
    fn end(&self) -> u64 {
        HEADER_LEN + u64::from(self.size) + self.footer()
    }
}

/// The size in a frame header, or `None` if it is not one.
fn frame_size(major: u8, h: &[u8; 10]) -> Option<u64> {
    if !h[..4]
        .iter()
        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    {
        return None;
    }
    if major == 4 {
        synchsafe(&h[4..8]).map(u64::from)
    } else {
        Some(u64::from(u32::from_be_bytes([h[4], h[5], h[6], h[7]])))
    }
}

fn frame_header(major: u8, ty: [u8; 4], len: u32) -> [u8; 10] {
    let size = if major == 4 {
        to_synchsafe(len)
    } else {
        len.to_be_bytes()
    };
    [
        ty[0], ty[1], ty[2], ty[3], size[0], size[1], size[2], size[3], 0, 0,
    ]
}

/// Whether the frame's format flags ask for nothing the frame body must be
/// decoded for: no compression, encryption, grouping, unsynchronisation or
/// data length indicator.
fn plain(major: u8, format_flags: u8) -> bool {
    if major == 3 {
        format_flags == 0
    } else {
        format_flags & 0x4f == 0
    }
}

/// Length of the string at the start of `s`, terminator included.
fn string_len(s: &[u8], encoding: u8) -> Option<usize> {
    if matches!(encoding, 1 | 2) {
        s.chunks_exact(2)
            .position(|c| c[0] == 0 && c[1] == 0)
            .map(|i| 2 * i + 2)
    } else {
        s.iter().position(|&b| b == 0).map(|i| i + 1)
    }
}

/// What the frame `id` with the start of its body `body` holds, and how many body
/// bytes come before the data, if it is C2PA or XMP.
fn classify(id: [u8; 4], body: &[u8]) -> Option<(ItemKind, usize)> {
    if id == GEOB {
        let encoding = *body.first()?;
        if encoding > 3 {
            return None;
        }
        let mime_len = body.get(1..)?.iter().position(|&b| b == 0)?;
        let mime = body.get(1..1 + mime_len)?;
        if mime != MIME && mime != MIME_DEPRECATED {
            return None;
        }
        let mut at = mime_len + 2;
        for _ in 0..2 {
            // File name, then description.
            at += string_len(body.get(at..)?, encoding)?;
        }
        Some((ItemKind::Jumbf, at))
    } else if id == PRIV && body.starts_with(PRIV_PREFIX) {
        Some((ItemKind::Xmp, PRIV_PREFIX.len()))
    } else {
        None
    }
}

fn frame_type(kind: ItemKind) -> Option<([u8; 4], &'static [u8])> {
    match kind {
        ItemKind::Jumbf => Some((GEOB, GEOB_PREFIX)),
        ItemKind::Xmp => Some((PRIV, PRIV_PREFIX)),
        _ => None,
    }
}

fn item_info(ty: [u8; 4], data: &[u8]) -> ItemInfo {
    if ty == GEOB {
        ItemInfo::jumbf().jumbf_type(superbox_type(data))
    } else {
        ItemInfo::xmp()
    }
}

// ── Parsing ──────────────────────────────────────────────────────────────────

fn is_mpeg_sync(b: &[u8; 4]) -> bool {
    // Eleven set bits, and a layer other than the reserved 00.
    b[0] == 0xff && b[1] & 0xe0 == 0xe0 && b[1] & 0x06 != 0
}

async fn parse_with(handler: Arc<dyn ContainerHandler>, mut io: Io) -> Result<Structure> {
    let total = io.len();
    if total < 4 {
        return Err(Error::InvalidFormat("ID3 audio too small".into()));
    }
    let mut r = io.reader(0);
    let tag = if &r.array::<3>().await? == b"ID3" {
        if total < HEADER_LEN {
            return Err(Error::InvalidFormat("truncated ID3 header".into()));
        }
        r.seek(0);
        Some(Tag::parse(&r.array::<10>().await?)?)
    } else {
        None
    };
    let end = tag.map_or(0, |t| t.end());
    if end > total {
        return Err(Error::invalid_segment(0, "ID3 tag extends past end of file"));
    }

    let lead = if total - end >= 4 {
        r.seek(end);
        r.array::<4>().await?
    } else {
        [0; 4]
    };
    let media = if &lead == FLAC_MARKER {
        FLAC
    } else if tag.is_some() || is_mpeg_sync(&lead) {
        MPEG
    } else {
        return Err(Error::UnsupportedFormat);
    };

    let mut b = StructureBuilder::new(handler, media, total);
    if let Some(tag) = tag {
        let body_end = HEADER_LEN + u64::from(tag.size);
        if tag.readable() {
            b.push_span(SegmentKind::Preamble, HEADER, 10, 0, 0)?;
            let mut pos = HEADER_LEN;
            while body_end - pos >= 10 {
                r.seek(pos);
                let fh = r.array::<10>().await?;
                let Some(size) = frame_size(tag.major, &fh) else {
                    break;
                };
                let len = 10 + size;
                if len > body_end - pos {
                    break;
                }
                let id = [fh[0], fh[1], fh[2], fh[3]];
                let candidate = (id == GEOB || id == PRIV) && size > 0 && plain(tag.major, fh[9]);
                let found = if candidate {
                    let n = size.min(PEEK as u64) as usize;
                    classify(id, r.take(n).await?).filter(|&(_, p)| (p as u64) < size)
                } else {
                    None
                };
                if let Some((kind, prefix)) = found {
                    let prefix = prefix as u64;
                    let run = pos - b.cursor();
                    if run > 0 {
                        b.push_span(SegmentKind::Other, Name::None, 0, run, 0)?;
                    }
                    let mut info = if kind == ItemKind::Xmp {
                        ItemInfo::xmp()
                    } else {
                        ItemInfo::jumbf()
                    };
                    if kind == ItemKind::Jumbf && size - prefix >= 32 {
                        r.seek(pos + 10 + prefix);
                        info = info.jumbf_type(superbox_type(r.take(32).await?));
                    }
                    b.push_item(
                        SegmentKind::Metadata,
                        Name::FourCC(id),
                        (10 + prefix) as u32,
                        size - prefix,
                        0,
                        info,
                    )?;
                }
                pos += len;
            }
            let run = pos - b.cursor();
            if run > 0 {
                b.push_span(SegmentKind::Other, Name::None, 0, run, 0)?;
            }
            if pos < body_end {
                b.push_span(SegmentKind::Other, PADDING, 0, body_end - pos, 0)?;
            }
            if tag.footer() > 0 {
                b.push_span(SegmentKind::Other, FOOTER, 10, 0, 0)?;
            }
        } else {
            b.push_span(SegmentKind::Preamble, OPAQUE, 10, 0, 0)?;
            if tag.end() > HEADER_LEN {
                b.push_span(SegmentKind::Other, Name::None, 0, tag.end() - HEADER_LEN, 0)?;
            }
        }
    }
    if total > end {
        b.push_span(SegmentKind::MediaData, Name::None, 0, total - end, 0)?;
    }
    b.finish()
}

// ── Writing ──────────────────────────────────────────────────────────────────

/// One piece of the output tag body.
enum Op<'a> {
    Copy(SegmentId),
    New {
        ty: [u8; 4],
        prefix: &'static [u8],
        data: &'a [u8],
    },
}

fn new_frame<'a>(kind: ItemKind, data: &'a [u8]) -> Result<Op<'a>> {
    let (ty, prefix) = frame_type(kind).ok_or(Error::UnsupportedFormat)?;
    Ok(Op::New { ty, prefix, data })
}

/// `head` is the source tag's header, when it has a readable one.
fn plan<'a>(src: &Structure, head: [u8; 10], updates: &'a Updates) -> Result<WritePlan<'a>> {
    let first = src.segments().first().map(|s| s.name());
    let opaque = first == Some(OPAQUE);
    let tagged = first == Some(HEADER);

    let mut xmp = MetadataSlot::new(updates.xmp());
    let mut jumbf = MetadataSlot::new(updates.jumbf());
    let (mut body, mut padding, mut media, mut changed) = (Vec::new(), None, None, false);
    if tagged {
        for (i, seg) in src.segments().iter().enumerate() {
            let id = SegmentId::from_index(i)?;
            let name = seg.name();
            if name == HEADER || name == FOOTER {
                continue; // rewritten, or dropped
            } else if name == PADDING {
                padding = Some(id);
                continue;
            }
            let slot = match (seg.kind(), src.items_in(id).next().map(|e| e.kind())) {
                (SegmentKind::MediaData, _) => {
                    media = Some(id);
                    continue;
                }
                (_, Some(ItemKind::Xmp)) => Some((ItemKind::Xmp, &mut xmp)),
                (_, Some(ItemKind::Jumbf)) => Some((ItemKind::Jumbf, &mut jumbf)),
                _ => None,
            };
            match slot {
                None => body.push(Op::Copy(id)),
                Some((kind, slot)) => match slot.existing() {
                    Existing::Keep => body.push(Op::Copy(id)),
                    Existing::Replace(d) => {
                        changed = true;
                        body.push(new_frame(kind, d)?);
                    }
                    _ => changed = true,
                },
            }
        }
    } else if let Some(m) = src.segments().len().checked_sub(1) {
        // A bare file or an opaque tag: nothing to rearrange.
        media = Some(SegmentId::from_index(m)?).filter(|_| !opaque);
    }
    for (kind, add) in [(ItemKind::Xmp, xmp.pending()), (ItemKind::Jumbf, jumbf.pending())] {
        if let Some(d) = add {
            if opaque {
                return Err(Error::UnsupportedFeature(
                    "ID3 tag with unsynchronisation, an extended header or unknown flags",
                ));
            }
            changed = true;
            body.push(new_frame(kind, d)?);
        }
    }

    let mut b = PlanBuilder::new(src);
    if !changed {
        for i in 0..src.segments().len() {
            b.copy_segment(SegmentId::from_index(i)?)?;
        }
        b.carry_items(|_| true);
        return b.finish();
    }

    let mut len = padding.map_or(Ok(0), |p| src.segment(p).map(|s| s.range().len()))?;
    for op in &body {
        let n = match op {
            Op::Copy(id) => src.segment(*id)?.range().len(),
            Op::New { prefix, data, .. } => 10 + prefix.len() as u64 + data.len() as u64,
        };
        len = len.saturating_add(n);
    }
    let size = u32::try_from(len).ok().filter(|&n| u64::from(n) <= MAX_TAG).ok_or(
        Error::LimitExceeded {
            what: "ID3 tag size",
            limit: MAX_TAG,
        },
    )?;

    // The source's version and flags, without a footer, or a new v2.4 tag.
    let mut hdr = *b"ID3\x04\0\0\0\0\0\0";
    if tagged {
        hdr = head;
        hdr[5] &= !0x10;
    }
    hdr[6..10].copy_from_slice(&to_synchsafe(size));
    b.put_segment(SegmentKind::Preamble, HEADER, hdr.to_vec(), &[][..], &[][..])?;
    for op in body {
        match op {
            Op::Copy(id) => {
                b.copy_segment(id)?;
            }
            Op::New { ty, prefix, data } => {
                let mut header = frame_header(hdr[3], ty, (prefix.len() + data.len()) as u32).to_vec();
                header.extend_from_slice(prefix);
                let id = b.put_segment(
                    SegmentKind::Metadata,
                    Name::FourCC(ty),
                    header,
                    data,
                    &[][..],
                )?;
                b.item(item_info(ty, data), &[id])?;
            }
        }
    }
    for id in [padding, media].into_iter().flatten() {
        b.copy_segment(id)?;
    }
    b.carry_items(|_| true);
    b.finish()
}

#[cfg(test)]
mod tests;
