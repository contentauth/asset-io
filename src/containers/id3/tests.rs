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

fn frame(major: u8, id: &[u8; 4], format_flags: u8, body: &[u8]) -> Vec<u8> {
    let len = body.len() as u32;
    let mut v = id.to_vec();
    v.extend_from_slice(&if major == 4 {
        to_synchsafe(len)
    } else {
        len.to_be_bytes()
    });
    v.extend_from_slice(&[0, format_flags]);
    v.extend_from_slice(body);
    v
}

fn geob(major: u8, mime: &str, data: &[u8]) -> Vec<u8> {
    let mut body = b"\0".to_vec(); // ISO-8859-1
    body.extend_from_slice(mime.as_bytes());
    body.extend_from_slice(b"\0name\0desc\0");
    body.extend_from_slice(data);
    frame(major, b"GEOB", 0, &body)
}

fn xmp_frame(major: u8, data: &[u8]) -> Vec<u8> {
    frame(major, b"PRIV", 0, &[b"XMP\0".as_slice(), data].concat())
}

fn text(major: u8) -> Vec<u8> {
    frame(major, b"TIT2", 0, b"\0title")
}

/// A tag of `frames` and `pad` zero bytes, with a footer if `flags` asks for one.
fn tag(major: u8, flags: u8, frames: &[Vec<u8>], pad: usize) -> Vec<u8> {
    let body = frames.iter().map(Vec::len).sum::<usize>() + pad;
    let mut v = b"ID3".to_vec();
    v.extend_from_slice(&[major, 0, flags]);
    v.extend_from_slice(&to_synchsafe(body as u32));
    frames.iter().for_each(|f| v.extend_from_slice(f));
    v.resize(v.len() + pad, 0);
    if major == 4 && flags & 0x10 != 0 {
        v.extend_from_slice(b"3DI");
        v.extend_from_slice(&[major, 0, flags]);
        v.extend_from_slice(&to_synchsafe(body as u32));
    }
    v
}

fn jumbf_stub(n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    v[0..4].copy_from_slice(&(n as u32).to_be_bytes());
    v[4..8].copy_from_slice(b"jumb");
    v
}

fn flac() -> Vec<u8> {
    let mut v = b"fLaC".to_vec();
    v.extend_from_slice(&[0x80, 0, 0, 34]);
    v.extend_from_slice(&[0; 34]);
    v.extend_from_slice(&[0xff, 0xf8, 1, 2, 3]);
    v
}

fn mp3() -> Vec<u8> {
    vec![
        0xff, 0xfb, 0x90, 0x00, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12,
    ]
}

fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
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

/// Where the audio starts in `bytes`, from the tag's size field.
fn audio_start(bytes: &[u8]) -> usize {
    10 + synchsafe(&bytes[6..10]).unwrap() as usize
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[test]
fn conforms() {
    let c = Conformance::new(handler())
        .jumbf(Some(jumbf_stub(64)))
        .xmp(Some(b"<x/>".to_vec()));
    let g = |m| geob(m, "application/c2pa", &jumbf_stub(48));
    c.check("bare mp3", &mp3());
    c.check("bare flac", &flac());
    c.check("empty v4 tag, flac", &cat(&[&tag(4, 0, &[], 0), &flac()]));
    c.check(
        "v4 tag with padding, flac",
        &cat(&[&tag(4, 0, &[text(4)], 16), &flac()]),
    );
    c.check(
        "v3 tag with manifest and xmp, mp3",
        &cat(&[
            &tag(3, 0, &[text(3), g(3), xmp_frame(3, b"<a/>")], 0),
            &mp3(),
        ]),
    );
    c.check(
        "v4 footer, flac",
        &cat(&[&tag(4, 0x10, &[g(4)], 8), &flac()]),
    );
}

#[test]
fn media_type_follows_the_audio() {
    assert_eq!(parse(&mp3()).unwrap().media_type(), "audio/mpeg");
    assert_eq!(parse(&flac()).unwrap().media_type(), "audio/flac");
    let t = tag(4, 0, &[], 0);
    assert_eq!(
        parse(&cat(&[&t, &flac()])).unwrap().media_type(),
        "audio/flac"
    );
    assert_eq!(
        parse(&cat(&[&t, &mp3()])).unwrap().media_type(),
        "audio/mpeg"
    );
    assert!(parse(b"not audio at all").is_err());
}

#[test]
fn finds_items_in_v3_and_v4() {
    for major in [3, 4] {
        let frames = [
            text(major),
            geob(major, "application/c2pa", b"manifest!"),
            xmp_frame(major, b"<x/>"),
        ];
        let f = cat(&[&tag(major, 0, &frames, 4), &flac()]);
        assert_eq!(items(&f, ItemKind::Jumbf), [b"manifest!".to_vec()]);
        assert_eq!(items(&f, ItemKind::Xmp), [b"<x/>".to_vec()]);
    }
}

#[test]
fn geob_mime_and_encoding() {
    let with = |g: Vec<u8>| cat(&[&tag(4, 0, &[g], 0), &mp3()]);
    let old = geob(4, "application/x-c2pa-manifest-store", b"old");
    assert_eq!(items(&with(old), ItemKind::Jumbf), [b"old".to_vec()]);
    let other = geob(4, "application/octet-stream", b"x");
    assert!(items(&with(other), ItemKind::Jumbf).is_empty());

    // UTF-16: the file name and description end in two NULs.
    let mut body = vec![1u8];
    body.extend_from_slice(b"application/c2pa\0");
    body.extend_from_slice(&[0xff, 0xfe, b'a', 0, 0, 0]); // file name
    body.extend_from_slice(&[0, 0]); // empty description
    body.extend_from_slice(b"utf16");
    let utf16 = frame(4, b"GEOB", 0, &body);
    assert_eq!(items(&with(utf16), ItemKind::Jumbf), [b"utf16".to_vec()]);
}

#[test]
fn data_hash_excludes_only_the_object_data() {
    let stub = jumbf_stub(64);
    let t = tag(4, 0, &[text(4), geob(4, "application/c2pa", &stub)], 0);
    let f = cat(&[&t, &mp3()]);
    let s = parse(&f).unwrap();
    let item = s.require_item(ItemKind::Jumbf).unwrap();
    let ex = s.c2pa_data_hash_exclusions(item).unwrap();
    assert_eq!(ex.len(), 1);
    assert_eq!(&f[ex[0].offset() as usize..ex[0].end() as usize], &stub[..]);
    assert_eq!(s.c2pa_hash(), Some(C2paHash::Data));
}

#[test]
fn footer_is_counted_then_dropped_on_write() {
    let f = cat(&[&tag(4, 0x10, &[text(4)], 6), &flac()]);
    assert_eq!(parse(&f).unwrap().media_type(), "audio/flac");

    let out = edit(&f, &Updates::new().set_jumbf(jumbf_stub(40))).unwrap();
    assert_eq!(out[5] & 0x10, 0);
    assert!(!out.windows(3).any(|w| w == b"3DI"));
    assert_eq!(items(&out, ItemKind::Jumbf), [jumbf_stub(40)]);
    let end = audio_start(&out);
    assert_eq!(&out[end..end + 4], b"fLaC");
}

#[test]
fn rejects_bad_tags() {
    let good = cat(&[&tag(4, 0, &[text(4)], 0), &mp3()]);

    let mut f = good.clone();
    f[6] = 0x80; // size is not synchsafe
    assert!(parse(&f).is_err());

    let mut f = good.clone();
    f[3] = 5; // no such version
    assert!(parse(&f).is_err());

    assert!(parse(&good[..14]).is_err()); // the tag runs past the end
}

#[test]
fn unreadable_tags_pass_through_untouched() {
    let hidden = geob(4, "application/c2pa", b"hidden");
    let cases = [
        cat(&[&tag(4, 0x80, std::slice::from_ref(&hidden), 0), &mp3()]), // unsynchronised
        cat(&[&tag(4, 0x40, std::slice::from_ref(&hidden), 0), &mp3()]), // extended header
        cat(&[&tag(4, 0x01, &[hidden], 0), &mp3()]),                     // unknown flag
        cat(&[&tag(2, 0, &[], 4), &mp3()]),                              // ID3v2.2
    ];
    for f in cases {
        assert_eq!(parse(&f).unwrap().items().count(), 0);
        assert_eq!(edit(&f, &Updates::new()).unwrap(), f);
        assert_eq!(edit(&f, &Updates::new().remove_jumbf()).unwrap(), f);
        assert!(edit(&f, &Updates::new().set_jumbf(jumbf_stub(40))).is_err());
    }
}

#[test]
fn encoded_frames_are_not_items() {
    let flagged = |major, flags| {
        let mut f = geob(major, "application/c2pa", b"x");
        f[9] = flags;
        cat(&[&tag(major, 0, &[f], 0), &mp3()])
    };
    // v2.4: compression, encryption, unsynchronisation, data length, grouping.
    for flags in [0x08, 0x04, 0x02, 0x01, 0x40] {
        assert!(items(&flagged(4, flags), ItemKind::Jumbf).is_empty());
    }
    // v2.3: compression, encryption, grouping.
    for flags in [0x80, 0x40, 0x20] {
        assert!(items(&flagged(3, flags), ItemKind::Jumbf).is_empty());
    }
}

#[test]
fn a_bad_frame_ends_the_frames() {
    let mut bad = frame(4, b"TIT2", 0, b"x");
    bad[4..8].copy_from_slice(&to_synchsafe(1000)); // runs past the tag
    let frames = [
        xmp_frame(4, b"<x/>"),
        bad,
        geob(4, "application/c2pa", b"later"),
    ];
    let f = cat(&[&tag(4, 0, &frames, 0), &mp3()]);
    assert_eq!(items(&f, ItemKind::Xmp).len(), 1);
    assert!(items(&f, ItemKind::Jumbf).is_empty());

    // New metadata goes ahead of the unparsed remainder, which is kept.
    let out = edit(&f, &Updates::new().set_jumbf(jumbf_stub(40))).unwrap();
    assert_eq!(items(&out, ItemKind::Jumbf), [jumbf_stub(40)]);
    assert_eq!(items(&out, ItemKind::Xmp).len(), 1);
    assert!(out.windows(5).any(|w| w == b"later"));
}

#[test]
fn set_replaces_the_first_in_place_and_drops_the_rest() {
    let one = geob(4, "application/c2pa", b"one");
    let two = geob(4, "application/c2pa", b"two");
    let f = cat(&[&tag(4, 0, &[one, text(4), two], 8), &flac()]);
    assert_eq!(items(&f, ItemKind::Jumbf).len(), 2);

    let out = edit(&f, &Updates::new().set_jumbf(jumbf_stub(40))).unwrap();
    assert_eq!(items(&out, ItemKind::Jumbf), [jumbf_stub(40)]);
    assert_eq!(&out[10..14], b"GEOB"); // still the first frame

    let gone = edit(&f, &Updates::new().remove_jumbf()).unwrap();
    assert!(items(&gone, ItemKind::Jumbf).is_empty());
    let title = text(4);
    assert!(gone.windows(title.len()).any(|w| w == &title[..]));
    assert_eq!(&gone[audio_start(&gone)..], &flac()[..]);
}

#[test]
fn a_tag_is_added_to_a_bare_file() {
    let f = flac();
    let updates = Updates::new()
        .set_jumbf(jumbf_stub(40))
        .set_xmp(b"<x/>".to_vec());
    let out = edit(&f, &updates).unwrap();
    assert_eq!(&out[..4], b"ID3\x04");
    assert_eq!(&out[audio_start(&out)..], &f[..]);
    assert_eq!(items(&out, ItemKind::Xmp), [b"<x/>".to_vec()]);

    let back = edit(&out, &Updates::new().remove_jumbf().remove_xmp()).unwrap();
    assert!(items(&back, ItemKind::Jumbf).is_empty());
    assert_eq!(&back[audio_start(&back)..], &f[..]);
}

#[test]
fn compose_is_the_frame_a_write_emits() {
    let data = jumbf_stub(40);
    let frame = handler().compose(ItemKind::Jumbf, &data).unwrap();
    let out = edit(&mp3(), &Updates::new().set_jumbf(data)).unwrap();
    assert_eq!(&out[10..10 + frame.len()], &frame[..]);
    assert!(handler().compose(ItemKind::Exif, b"x").is_err());
}

#[test]
fn patch_fills_the_placeholder_in_place() {
    let f = cat(&[&tag(4, 0, &[text(4)], 0), &flac()]);
    let s = parse(&f).unwrap();
    let mut out = Cursor::new(Vec::new());
    let dest = write(
        &s,
        &mut Cursor::new(f),
        &mut out,
        &Updates::new().set_jumbf(vec![0u8; 64]),
        &mut NullSink,
    )
    .unwrap();
    let item = dest.require_item(ItemKind::Jumbf).unwrap().clone();
    dest.patch(&mut out, &item, b"signed").unwrap();
    assert!(dest.patch(&mut out, &item, &[1u8; 65]).is_err());

    let got = items(&out.into_inner(), ItemKind::Jumbf);
    assert_eq!(got[0].len(), 64);
    assert_eq!(&got[0][..6], b"signed");
}

#[test]
fn real_file_round_trip() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample1.mp3");
    let Ok(f) = std::fs::read(path) else { return };
    let updates = Updates::new()
        .set_jumbf(jumbf_stub(256))
        .set_xmp(b"<x/>".to_vec());
    let out = edit(&f, &updates).unwrap();
    assert_eq!(items(&out, ItemKind::Jumbf), [jumbf_stub(256)]);
    let back = edit(&out, &Updates::new().remove_jumbf().remove_xmp()).unwrap();
    assert!(back.ends_with(&f[f.len() - 1000..]));
}
