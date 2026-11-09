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

use std::io::Cursor;

use super::*;
use crate::{
    testing::{write, Conformance},
    NullSink,
};

// ── Builders ─────────────────────────────────────────────────────────────────

/// Header, a 1x1 screen with a two-entry global color table, `blocks`, trailer.
fn gif(version: &[u8; 6], blocks: &[Vec<u8>]) -> Vec<u8> {
    let mut v = version.to_vec();
    v.extend_from_slice(&[1, 0, 1, 0, 0x80, 0, 0]);
    v.extend_from_slice(&[0, 0, 0, 255, 255, 255]);
    blocks.iter().for_each(|b| v.extend_from_slice(b));
    v.push(TRAILER);
    v
}

const SCREEN_END: usize = 19;

fn image() -> Vec<u8> {
    let mut v = vec![IMAGE, 0, 0, 0, 0, 1, 0, 1, 0, 0];
    v.extend_from_slice(&[2, 2, 0x4c, 0x01, 0]);
    v
}

fn control() -> Vec<u8> {
    vec![EXTENSION, 0xf9, 4, 0, 0, 0, 0, 0]
}

fn comment(text: &[u8]) -> Vec<u8> {
    let mut v = vec![EXTENSION, 0xfe, text.len() as u8];
    v.extend_from_slice(text);
    v.push(0);
    v
}

fn netscape() -> Vec<u8> {
    let mut v = vec![EXTENSION, APPLICATION, 11];
    v.extend_from_slice(b"NETSCAPE2.0");
    v.extend_from_slice(&[3, 1, 0, 0, 0]);
    v
}

fn block(kind: ItemKind, data: &[u8]) -> Vec<u8> {
    handler().compose(kind, data).unwrap()
}

fn jumbf_stub(n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    v[0..4].copy_from_slice(&(n as u32).to_be_bytes());
    v[4..8].copy_from_slice(b"jumb");
    v
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn parse(bytes: &[u8]) -> Result<Structure> {
    handler().parse(&mut Cursor::new(bytes.to_vec()))
}

fn edit(bytes: &[u8], updates: &Updates) -> Result<Vec<u8>> {
    let s = parse(bytes)?;
    let mut out = Cursor::new(Vec::new());
    write(
        &s,
        &mut Cursor::new(bytes.to_vec()),
        &mut out,
        updates,
        &mut NullSink,
    )?;
    Ok(out.into_inner())
}

fn items(bytes: &[u8], kind: ItemKind) -> Vec<Vec<u8>> {
    let s = parse(bytes).unwrap();
    s.items_of(kind)
        .map(|i| s.read_item(&mut Cursor::new(bytes.to_vec()), i).unwrap())
        .collect()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[test]
fn conforms() {
    let c = Conformance::new(handler())
        .jumbf(Some(jumbf_stub(600)))
        .xmp(Some(b"<x:xmpmeta/>".to_vec()));
    c.check("89a", &gif(b"GIF89a", &[image()]));
    c.check("87a", &gif(b"GIF87a", &[image()]));
    c.check(
        "extensions",
        &gif(
            b"GIF89a",
            &[netscape(), control(), comment(b"hi"), image(), image()],
        ),
    );
    c.check(
        "with c2pa and xmp",
        &gif(
            b"GIF89a",
            &[
                block(ItemKind::Jumbf, &jumbf_stub(300)),
                block(ItemKind::Xmp, b"<x/>"),
                image(),
            ],
        ),
    );
    let mut trailing = gif(b"GIF89a", &[image()]);
    trailing.extend_from_slice(b"junk");
    c.check("trailing bytes", &trailing);
}

#[test]
fn compose_matches_c2pa_rs() {
    // The block c2pa-rs composes for the manifest [1, 2, 3].
    assert_eq!(
        block(ItemKind::Jumbf, &[1, 2, 3]),
        [33, 255, 11, 67, 50, 80, 65, 95, 71, 73, 70, 1, 0, 0, 3, 1, 2, 3, 0]
    );
    assert!(handler().compose(ItemKind::Exif, b"x").is_err());
}

#[test]
fn items_round_trip_across_sub_blocks() {
    let g = gif(b"GIF89a", &[image()]);
    let store = jumbf_stub(1000); // four sub-blocks
    let packet = b"<x:xmpmeta>packet</x:xmpmeta>".repeat(20);
    let updates = Updates::new()
        .set_jumbf(store.clone())
        .set_xmp(packet.clone());
    let out = edit(&g, &updates).unwrap();
    assert_eq!(items(&out, ItemKind::Jumbf), [store]);
    assert_eq!(items(&out, ItemKind::Xmp), [packet]);
    // New blocks follow the global color table, C2PA first.
    assert_eq!(&out[SCREEN_END..SCREEN_END + 3], &[EXTENSION, APPLICATION, 11]);
    assert_eq!(&out[SCREEN_END + 3..SCREEN_END + 14], C2PA_APP);
}

#[test]
fn reads_xmp_written_by_c2pa_rs() {
    // c2pa-rs chunks the packet and the trailer together.
    let packet = b"<x:xmpmeta/>".repeat(30);
    let mut stored = packet.clone();
    stored.extend_from_slice(&XMP_TRAILER);
    let mut xmp = vec![EXTENSION, APPLICATION, 11];
    xmp.extend_from_slice(XMP_APP);
    for c in stored.chunks(255) {
        xmp.push(c.len() as u8);
        xmp.extend_from_slice(c);
    }
    xmp.push(0);
    let g = gif(b"GIF89a", &[xmp, image()]);
    assert_eq!(items(&g, ItemKind::Xmp), [packet]);

    // Kept as it is, it keeps its extents.
    let out = edit(&g, &Updates::new().set_jumbf(jumbf_stub(64))).unwrap();
    assert_eq!(items(&out, ItemKind::Xmp).len(), 1);
}

#[test]
fn xmp_without_its_trailer_is_not_an_item() {
    let mut bad = block(ItemKind::Xmp, b"<x/>");
    let at = bad.len() - 2; // last trailer byte
    bad[at] = 9;
    let short = {
        let mut v = vec![EXTENSION, APPLICATION, 11];
        v.extend_from_slice(XMP_APP);
        v.extend_from_slice(&[10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        v
    };
    for xmp in [bad, short] {
        let g = gif(b"GIF89a", &[xmp, image()]);
        assert!(items(&g, ItemKind::Xmp).is_empty());
        assert_eq!(edit(&g, &Updates::new()).unwrap(), g);
    }
}

#[test]
fn replace_in_place_and_remove_restores_the_original() {
    let g = gif(b"GIF89a", &[netscape(), image()]);
    let signed = edit(&g, &Updates::new().set_jumbf(jumbf_stub(300))).unwrap();
    let again = edit(&signed, &Updates::new().set_jumbf(jumbf_stub(40))).unwrap();
    assert_eq!(items(&again, ItemKind::Jumbf), [jumbf_stub(40)]);
    assert_eq!(&again[SCREEN_END + 3..SCREEN_END + 14], C2PA_APP);
    assert_eq!(
        edit(&again, &Updates::new().remove_jumbf()).unwrap(),
        g
    );
}

#[test]
fn a_second_store_is_dropped_on_write() {
    let one = block(ItemKind::Jumbf, &jumbf_stub(64));
    let two = block(ItemKind::Jumbf, &jumbf_stub(80));
    let g = gif(b"GIF89a", &[one, image(), two]);
    assert_eq!(items(&g, ItemKind::Jumbf).len(), 2);
    let out = edit(&g, &Updates::new().set_jumbf(jumbf_stub(50))).unwrap();
    assert_eq!(items(&out, ItemKind::Jumbf), [jumbf_stub(50)]);
}

#[test]
fn gif87a_becomes_89a_only_when_a_block_is_added() {
    let g = gif(b"GIF87a", &[image()]);
    assert_eq!(edit(&g, &Updates::new()).unwrap(), g);
    let out = edit(&g, &Updates::new().set_jumbf(jumbf_stub(64))).unwrap();
    assert_eq!(&out[..6], b"GIF89a");
}

#[test]
fn data_hash_excludes_the_whole_block() {
    let manifest = block(ItemKind::Jumbf, &jumbf_stub(600));
    let g = gif(b"GIF89a", &[manifest.clone(), image()]);
    let s = parse(&g).unwrap();
    assert_eq!(s.c2pa_hash(), Some(C2paHash::Data));
    let ex = s
        .c2pa_data_hash_exclusions(s.require_item(ItemKind::Jumbf).unwrap())
        .unwrap();
    assert_eq!(ex.len(), 1);
    assert_eq!(
        (ex[0].offset(), ex[0].len()),
        (SCREEN_END as u64, manifest.len() as u64)
    );
}

#[test]
fn patch_exact_fills_the_placeholder() {
    let g = gif(b"GIF89a", &[image()]);
    let s = parse(&g).unwrap();
    let mut out = Cursor::new(Vec::new());
    let dest = write(
        &s,
        &mut Cursor::new(g),
        &mut out,
        &Updates::new().set_jumbf(vec![0u8; 700]),
        &mut NullSink,
    )
    .unwrap();
    let item = dest.require_item(ItemKind::Jumbf).unwrap().clone();
    let signed: Vec<u8> = (0..700).map(|i| i as u8).collect();
    dest.patch_exact(&mut out, &item, &signed).unwrap();
    assert!(dest.patch_exact(&mut out, &item, &signed[..699]).is_err());
    assert_eq!(items(&out.into_inner(), ItemKind::Jumbf), [signed]);
}

#[test]
fn unknown_extensions_are_skipped() {
    let odd = vec![EXTENSION, 0x99, 2, 7, 7, 0];
    let g = gif(b"GIF89a", &[odd, image()]);
    assert!(parse(&g).is_ok());
}

#[test]
fn rejects_bad_files() {
    assert!(parse(b"GIF90a\0\0\0\0\0\0\0").is_err());
    let g = gif(b"GIF89a", &[image()]);
    assert!(parse(&g[..g.len() - 1]).is_err()); // no trailer
    let mut odd = g.clone();
    odd[SCREEN_END] = 0x42; // no such block
    assert!(parse(&odd).is_err());
    let mut long = gif(b"GIF89a", &[comment(b"abc")]);
    long[SCREEN_END + 2] = 200; // sub-block runs past the end
    assert!(parse(&long).is_err());
}

#[test]
fn real_file_round_trip() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample1.gif");
    let Ok(g) = std::fs::read(path) else { return };
    let updates = Updates::new()
        .set_jumbf(jumbf_stub(5000))
        .set_xmp(b"<x:xmpmeta/>".to_vec());
    let out = edit(&g, &updates).unwrap();
    assert_eq!(items(&out, ItemKind::Jumbf), [jumbf_stub(5000)]);
    let back = edit(&out, &Updates::new().remove_jumbf().remove_xmp()).unwrap();
    assert_eq!(back, g);
}
