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

//! GIF (87a and 89a) on the structure model.
//!
//! The header, the logical screen descriptor (with its global color table) and
//! each block are segments; an image is its descriptor (with any local color
//! table) and its image data. C2PA is an application extension identified
//! `C2PA_GIF` with authentication code `01 00 00`, XMP one identified `XMP Data`
//! with code `XMP`.
//!
//! Application data is stored in sub-blocks of at most 255 bytes, each after a
//! length byte. So that an item is whole segment payloads, a C2PA or XMP block
//! is one segment per sub-block: the length byte (and, for the first, the
//! 14-byte block introduction) is the header, the data the payload, and the
//! block's `00` terminator the last segment's trailer. Other blocks are one
//! segment each.
//!
//! XMP ends in the 257-byte "magic trailer" the XMP specification requires for
//! GIF; the XMP item stops before it, and an XMP block without it is not an
//! item. A write stores the trailer in sub-blocks of its own.
//!
//! A write replaces an existing item in place. A new block goes after the
//! global color table, and a GIF87a header becomes GIF89a.
//!
//! The C2PA hard binding is a box hash with the boxes c2pa-rs lists: the header
//! (`GIF89a`), the logical screen descriptor with its global color table
//! (`LSD`), each extension (`21` and its label in hex, such as `21F9`), each
//! image descriptor with its local color table (`2C`), its image data (`TBID`)
//! and the trailer (`3B`). An application extension is one box, whatever its
//! sub-blocks. A data hash excluding the whole C2PA block is also supported.

use std::{borrow::Cow, sync::Arc};

use crate::{
    container_api::{
        BoxFuture, ContainerHandler, Existing, FormatSpec, Io, ItemInfo, MetadataSlot, PlanBuilder,
        StructureBuilder, WritePlan,
    },
    io::Reader,
    jumbf::superbox_type,
    ByteRange, Error, ItemKind, Name, Result, Segment, SegmentId, SegmentKind, Structure, Updates,
};

/// The GIF container's [`ContainerHandler::id`].
pub const ID: &str = "gif";

const MEDIA_TYPE: &str = "image/gif";

const EXTENSION: u8 = 0x21;
const APPLICATION: u8 = 0xff;
const IMAGE: u8 = 0x2c;
const TRAILER: u8 = 0x3b;

const HEADER: Name = Name::FourCC(*b"GIFh");
const SCREEN: Name = Name::FourCC(*b"LSD ");
const IMAGE_DATA: Name = Name::FourCC(*b"TBID");

/// An extension block, named as a C2PA box hash lists it: `21` and the label in
/// hex, such as `21F9`.
const fn extension(label: u8) -> Name {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    Name::FourCC([
        b'2',
        b'1',
        HEX[(label >> 4) as usize],
        HEX[(label & 0xf) as usize],
    ])
}

/// Application identifier and authentication code.
const C2PA_APP: &[u8; 11] = b"C2PA_GIF\x01\x00\x00";
const XMP_APP: &[u8; 11] = b"XMP DataXMP";
/// `21 FF 0B`, then the identifier and code.
const INTRO_LEN: u32 = 14;
const MAX_SUB_BLOCK: usize = 255;

/// Each length byte, to borrow as a one-byte header.
static LENGTHS: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = i as u8;
        i += 1;
    }
    t
};

/// `01 FF FE ... 01 00`.
static XMP_TRAILER: [u8; 257] = {
    let mut t = [0u8; 257];
    t[0] = 1;
    let mut i = 1;
    while i < 257 {
        t[i] = (256 - i) as u8;
        i += 1;
    }
    t
};

/// The GIF container handler.
#[derive(Debug, Clone, Copy, Default)]
pub struct GifHandler;

/// The shared GIF handler.
pub fn handler() -> Arc<dyn ContainerHandler> {
    Arc::new(GifHandler)
}

impl ContainerHandler for GifHandler {
    fn id(&self) -> &'static str {
        ID
    }

    fn formats(&self) -> &[FormatSpec] {
        const F: &[FormatSpec] = &[FormatSpec::new(MEDIA_TYPE, "gif")];
        F
    }

    fn detect(&self, header: &[u8]) -> bool {
        header.starts_with(b"GIF87a") || header.starts_with(b"GIF89a")
    }

    // The names c2pa-rs lists. It names the header `GIF89a` for either version.
    fn box_name(
        &self,
        _structure: &Structure,
        _segment: SegmentId,
        element: &Segment,
    ) -> Option<Cow<'static, str>> {
        let name = element.name();
        if name == HEADER {
            return Some(Cow::Borrowed("GIF89a"));
        }
        if name == SCREEN {
            return Some(Cow::Borrowed("LSD"));
        }
        match name {
            Name::None => None,
            Name::Marker(m) => Some(Cow::Owned(format!("{m:02X}"))),
            other => Some(Cow::Owned(other.to_string())),
        }
    }

    // The data sub-blocks after an application extension's first are part of its box.
    fn box_continues(&self, _: &Structure, _: SegmentId, element: &Segment) -> bool {
        element.kind() == SegmentKind::Metadata && element.name().is_none()
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
            let mut head = [0u8; 6];
            io.read_at(0, &mut head).await?;
            plan(src, &head == b"GIF87a", updates)
        })
    }

    fn compose(&self, kind: ItemKind, data: &[u8]) -> Result<Vec<u8>> {
        let id = app_id(kind).ok_or(Error::UnsupportedFormat)?;
        let mut out = Vec::with_capacity(16 + data.len() + data.len() / MAX_SUB_BLOCK + 260);
        out.extend_from_slice(&[EXTENSION, APPLICATION, 11]);
        out.extend_from_slice(id);
        for (chunk, _) in sub_blocks(kind, data) {
            out.push(chunk.len() as u8);
            out.extend_from_slice(chunk);
        }
        out.push(0);
        Ok(out)
    }
}

fn app_id(kind: ItemKind) -> Option<&'static [u8; 11]> {
    match kind {
        ItemKind::Jumbf => Some(C2PA_APP),
        ItemKind::Xmp => Some(XMP_APP),
        _ => None,
    }
}

/// The sub-blocks a block holding `data` stores, and whether each is content
/// (not the XMP trailer).
fn sub_blocks(kind: ItemKind, data: &[u8]) -> impl Iterator<Item = (&[u8], bool)> {
    let trailer: &'static [u8] = if kind == ItemKind::Xmp {
        &XMP_TRAILER
    } else {
        &[]
    };
    data.chunks(MAX_SUB_BLOCK)
        .map(|c| (c, true))
        .chain(trailer.chunks(MAX_SUB_BLOCK).map(|c| (c, false)))
}

/// Size of the color table a packed field declares.
fn color_table(packed: u8) -> u64 {
    if packed & 0x80 != 0 {
        3 << ((packed & 7) + 1)
    } else {
        0
    }
}

// ── Parsing ──────────────────────────────────────────────────────────────────

async fn parse_with(handler: Arc<dyn ContainerHandler>, mut io: Io) -> Result<Structure> {
    let total = io.len();
    if total < 13 {
        return Err(Error::InvalidFormat("GIF too small".into()));
    }
    let mut r = io.reader(0);
    let head = r.array::<13>().await?;
    if &head[..6] != b"GIF87a" && &head[..6] != b"GIF89a" {
        return Err(Error::InvalidFormat("Not a GIF file".into()));
    }
    let mut b = StructureBuilder::new(handler, MEDIA_TYPE, total);
    b.push_span(SegmentKind::Preamble, HEADER, 6, 0, 0)?;
    b.push_span(SegmentKind::Preamble, SCREEN, 7, color_table(head[10]), 0)?;

    loop {
        let pos = b.cursor();
        match read_byte(&mut r, pos, total).await? {
            TRAILER => {
                b.push_span(SegmentKind::Other, Name::Marker(TRAILER), 1, 0, 0)?;
                break;
            }
            IMAGE => {
                r.seek(pos);
                let d = r.array::<10>().await?;
                b.push_span(
                    SegmentKind::MediaData,
                    Name::Marker(IMAGE),
                    10,
                    color_table(d[9]),
                    0,
                )?;
                // LZW minimum code size, then sub-blocks.
                let at = b.cursor();
                let end = skip_sub_blocks(&mut r, at + 1, total).await?;
                b.push_span(SegmentKind::MediaData, IMAGE_DATA, 1, end - at - 1, 0)?;
            }
            EXTENSION => {
                let label = read_byte(&mut r, pos + 1, total).await?;
                let kind = if label == APPLICATION {
                    app_kind(&mut r, pos + 2, total).await?
                } else {
                    None
                };
                match kind {
                    Some(kind) => app_block(&mut b, &mut r, kind, pos, total).await?,
                    None => {
                        // Any extension, known or not, is a label and sub-blocks.
                        let end = skip_sub_blocks(&mut r, pos + 2, total).await?;
                        b.push_span(SegmentKind::Other, extension(label), 2, end - pos - 2, 0)?;
                    }
                }
            }
            other => {
                return Err(Error::invalid_segment(
                    pos,
                    format!("unknown GIF block 0x{other:02x}"),
                ))
            }
        }
    }

    // The blocks through the trailer are the structure; anything after is data beyond it.
    b.end_boxes()?;
    b.push_tail()?;
    b.finish()
}

async fn read_byte(r: &mut Reader<'_>, at: u64, total: u64) -> Result<u8> {
    if at >= total {
        return Err(Error::invalid_segment(at, "GIF truncated"));
    }
    r.seek(at);
    r.u8().await
}

/// Where the sub-blocks starting at `at` end, after their terminator.
async fn skip_sub_blocks(r: &mut Reader<'_>, mut at: u64, total: u64) -> Result<u64> {
    loop {
        let n = read_byte(r, at, total).await?;
        at += 1 + u64::from(n);
        if n == 0 {
            return Ok(at);
        }
    }
}

/// Whether the application extension whose block size byte is at `at` is C2PA or XMP.
async fn app_kind(r: &mut Reader<'_>, at: u64, total: u64) -> Result<Option<ItemKind>> {
    if total.saturating_sub(at) < 12 {
        return Ok(None);
    }
    r.seek(at);
    let a = r.array::<12>().await?;
    Ok(match (a[0], &a[1..]) {
        (11, id) if id == C2PA_APP => Some(ItemKind::Jumbf),
        (11, id) if id == XMP_APP => Some(ItemKind::Xmp),
        _ => None,
    })
}

/// A C2PA or XMP block at `pos`, one segment per sub-block, and its item.
async fn app_block(
    b: &mut StructureBuilder,
    r: &mut Reader<'_>,
    kind: ItemKind,
    pos: u64,
    total: u64,
) -> Result<()> {
    let mut at = pos + u64::from(INTRO_LEN);
    let mut n = read_byte(r, at, total).await?;
    if n == 0 {
        b.push_span(
            SegmentKind::Metadata,
            extension(APPLICATION),
            INTRO_LEN,
            0,
            1,
        )?;
        return Ok(());
    }
    let mut parts = Vec::new();
    loop {
        let next_at = at + 1 + u64::from(n);
        let next = read_byte(r, next_at, total).await?;
        let (name, header) = match parts.is_empty() {
            true => (extension(APPLICATION), INTRO_LEN + 1),
            false => (Name::None, 1),
        };
        let id = b.push_span(
            SegmentKind::Metadata,
            name,
            header,
            u64::from(n),
            u16::from(next == 0),
        )?;
        parts.push((id, b.segment(id)?.payload()));
        if next == 0 {
            break;
        }
        (at, n) = (next_at, next);
    }

    if kind == ItemKind::Jumbf {
        let first = parts[0].1;
        let ty = if first.len() >= 32 {
            r.seek(first.offset());
            superbox_type(r.take(32).await?)
        } else {
            None
        };
        return b.item(ItemInfo::jumbf().jumbf_type(ty), &parts);
    }
    match xmp_extents(r, &parts).await? {
        Some(extents) => b.item(ItemInfo::xmp(), &extents),
        None => Ok(()),
    }
}

/// The extents of the XMP packet in `parts`, without the magic trailer, or `None`
/// when the trailer is missing or nothing precedes it.
async fn xmp_extents(
    r: &mut Reader<'_>,
    parts: &[(SegmentId, ByteRange)],
) -> Result<Option<Vec<(SegmentId, ByteRange)>>> {
    let len: u64 = parts.iter().map(|(_, p)| p.len()).sum();
    let trailer = XMP_TRAILER.len();
    if len <= trailer as u64 {
        return Ok(None);
    }
    let mut tail = [0u8; 257];
    let mut need = trailer;
    for (_, p) in parts.iter().rev() {
        if need == 0 {
            break;
        }
        let take = (p.len() as usize).min(need);
        r.seek(p.end() - take as u64);
        r.read_exact(&mut tail[need - take..need]).await?;
        need -= take;
    }
    if tail != XMP_TRAILER {
        return Ok(None);
    }
    let mut keep = len - trailer as u64;
    let mut extents = Vec::new();
    for &(id, p) in parts {
        if keep == 0 {
            break;
        }
        let take = p.len().min(keep);
        extents.push((id, p.sub(0, take)?));
        keep -= take;
    }
    Ok(Some(extents))
}

// ── Writing ──────────────────────────────────────────────────────────────────

enum Op<'a> {
    Copy(SegmentId),
    New(ItemKind, &'a [u8]),
}

fn plan<'a>(src: &Structure, gif87a: bool, updates: &'a Updates) -> Result<WritePlan<'a>> {
    let segs = src.segments();
    let mut xmp = MetadataSlot::new(updates.xmp());
    let mut jumbf = MetadataSlot::new(updates.jumbf());
    let mut ops = Vec::with_capacity(segs.len() + 2);
    let (mut changed, mut after_screen) = (false, None);

    let mut i = 0;
    while i < segs.len() {
        let id = SegmentId::from_index(i)?;
        if segs[i].kind() != SegmentKind::Metadata {
            ops.push(Op::Copy(id));
            if segs[i].name() == SCREEN {
                after_screen = Some(ops.len());
            }
            i += 1;
            continue;
        }
        // A C2PA or XMP block: its segments through the one ending in the terminator.
        let mut end = i;
        while segs[end].trailer().is_empty() && end + 1 < segs.len() {
            end += 1;
        }
        let slot = match src.items_in(id).next().map(|e| e.kind()) {
            Some(ItemKind::Xmp) => Some((ItemKind::Xmp, &mut xmp)),
            Some(ItemKind::Jumbf) => Some((ItemKind::Jumbf, &mut jumbf)),
            _ => None,
        };
        match slot.map(|(kind, s)| (kind, s.existing())) {
            None | Some((_, Existing::Keep)) => {
                for k in i..=end {
                    ops.push(Op::Copy(SegmentId::from_index(k)?));
                }
            }
            Some((kind, Existing::Replace(d))) => {
                changed = true;
                ops.push(Op::New(kind, d));
            }
            Some(_) => changed = true,
        }
        i = end + 1;
    }

    let adds: Vec<Op<'a>> = [
        (ItemKind::Jumbf, jumbf.pending()),
        (ItemKind::Xmp, xmp.pending()),
    ]
    .into_iter()
    .filter_map(|(kind, d)| d.filter(|d| !d.is_empty()).map(|d| Op::New(kind, d)))
    .collect();
    if !adds.is_empty() {
        let at = after_screen.ok_or(Error::NotFound {
            what: "GIF logical screen descriptor",
        })?;
        changed = true;
        let rest = ops.split_off(at);
        ops.extend(adds);
        ops.extend(rest);
    }

    let mut b = PlanBuilder::new(src);
    if !changed {
        for i in 0..segs.len() {
            b.copy_segment(SegmentId::from_index(i)?)?;
        }
        b.carry_items(|_| true);
        return b.finish();
    }
    // Extensions need GIF89a.
    let upgrade = gif87a && ops.iter().any(|op| matches!(op, Op::New(..)));
    for op in ops {
        match op {
            Op::Copy(id) if upgrade && id.index() == 0 => {
                b.put_segment(
                    SegmentKind::Preamble,
                    HEADER,
                    &b"GIF89a"[..],
                    &[][..],
                    &[][..],
                )?;
            }
            Op::Copy(id) => {
                b.copy_segment(id)?;
            }
            Op::New(kind, data) => put_block(&mut b, kind, data)?,
        }
    }
    b.carry_items(|_| true);
    b.finish()
}

/// Add a C2PA or XMP block holding `data`, one segment per sub-block, and its item.
/// The data is borrowed, not copied. Empty data adds nothing.
fn put_block<'a>(b: &mut PlanBuilder<'_, 'a>, kind: ItemKind, data: &'a [u8]) -> Result<()> {
    let app = app_id(kind).ok_or(Error::UnsupportedFormat)?;
    if data.is_empty() {
        return Ok(());
    }
    let parts: Vec<(&'a [u8], bool)> = sub_blocks(kind, data).collect();
    let mut content = Vec::with_capacity(parts.len());
    for (i, &(chunk, is_content)) in parts.iter().enumerate() {
        let n = chunk.len();
        let (name, header): (Name, Cow<'a, [u8]>) = if i == 0 {
            let mut h = vec![EXTENSION, APPLICATION, 11];
            h.extend_from_slice(app);
            h.push(n as u8);
            (extension(APPLICATION), Cow::Owned(h))
        } else {
            (Name::None, Cow::Borrowed(&LENGTHS[n..=n]))
        };
        let trailer: &'static [u8] = if i + 1 == parts.len() { &[0] } else { &[] };
        let id = b.put_segment(SegmentKind::Metadata, name, header, chunk, trailer)?;
        if is_content {
            content.push(id);
        }
    }
    let info = match kind {
        ItemKind::Xmp => ItemInfo::xmp(),
        _ => ItemInfo::jumbf(),
    };
    b.item(info, &content)
}

#[cfg(test)]
mod tests;
