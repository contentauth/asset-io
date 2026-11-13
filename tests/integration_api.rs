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

//! The pieces an embedding SDK needs beyond read and write: planning without
//! writing, the store as a reader sees it, and media types by format string.
#![cfg(all(feature = "jpeg", feature = "png", feature = "bmff", feature = "riff"))]

use std::{fs, io::Cursor, path::PathBuf};

use asset_io::{
    container_api, container_api::ItemInfo, testing::c2pa_store, Asset, HandlerRegistry, ItemKind,
    NullSink, Structure, Updates,
};

/// `bytes` as an asset, its container detected.
fn open<'a>(reg: &HandlerRegistry, bytes: &'a [u8]) -> asset_io::Result<Asset<Cursor<&'a [u8]>>> {
    Asset::open_with(reg, Cursor::new(bytes), None)
}

fn fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut v: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            let ext = p.extension()?.to_str()?.to_ascii_lowercase();
            [
                "jpg", "jpeg", "png", "webp", "wav", "heic", "heif", "avif", "m4a", "mp4", "mov",
            ]
            .contains(&ext.as_str())
            .then(|| {
                (
                    p.file_name().unwrap().to_string_lossy().into_owned(),
                    fs::read(&p).unwrap(),
                )
            })
        })
        .collect();
    v.sort();
    v
}

/// An item as a caller observes it: what it is, and its extents as
/// (segment index, offset, length).
type ItemShape = (ItemInfo, Vec<(usize, u64, u64)>);

/// Everything about a structure a caller can observe.
fn shape(s: &Structure) -> (u64, Vec<asset_io::Segment>, Vec<ItemShape>) {
    let mut items: Vec<ItemShape> = s
        .items()
        .map(|e| {
            let extents = e
                .extents()
                .map(|p| (p.segment().index(), p.range().offset(), p.range().len()))
                .collect();
            (container_api::item_info(e).clone(), extents)
        })
        .collect();
    // Declaration order is the handler's business; file order is what a caller sees.
    items.sort_by_key(|(_, extents)| extents.first().copied());
    (s.total_size(), s.segments().to_vec(), items)
}

#[test]
fn plan_is_what_write_returns() {
    let reg = HandlerRegistry::builtin();
    let updates = [
        Updates::new(),
        Updates::new().set_c2pa(c2pa_store(&[b"one"])),
        Updates::new()
            .set_c2pa(c2pa_store(&[b"one"]))
            .set_xmp(b"<x:xmpmeta/>\n".to_vec()),
        Updates::remove_all(),
        Updates::new().remove_c2pa().remove_xmp(),
    ];
    let mut ran = 0;
    for (name, bytes) in fixtures() {
        let Ok(s) = open(&reg, &bytes) else {
            continue;
        };
        for (i, u) in updates.iter().enumerate() {
            let planned = s.plan(u);
            let mut out = Vec::new();
            let written = s.write(&mut out, u, &mut NullSink);
            match (planned, written) {
                (Ok(p), Ok(w)) => {
                    assert_eq!(shape(&p), shape(&w), "{name} update {i}");
                    assert_eq!(p.total_size(), out.len() as u64, "{name} update {i}");
                }
                (Err(_), Err(_)) => {}
                (p, w) => panic!(
                    "{name} update {i}: plan {:?}, write {:?}",
                    p.is_ok(),
                    w.is_ok()
                ),
            }
        }
        ran += 1;
    }
    assert!(ran > 0, "no fixtures");
}

#[test]
fn plan_places_a_manifest_that_is_not_there_yet() {
    let reg = HandlerRegistry::builtin();
    let u = Updates::new().set_c2pa(c2pa_store(&[b"one"]));
    let mut ran = 0;
    for (name, bytes) in fixtures() {
        let Ok(s) = open(&reg, &bytes) else {
            continue;
        };
        if s.item(ItemKind::C2pa).ok().flatten().is_some() {
            continue;
        }
        let Ok(dest) = s.plan(&u) else {
            continue;
        };
        let e = dest.require_item(ItemKind::C2pa).unwrap();
        assert_eq!(e.len(), c2pa_store(&[b"one"]).len() as u64, "{name}");
        assert!(dest.total_size() > s.total_size(), "{name}");
        ran += 1;
    }
    assert!(ran > 0, "no fixture without a manifest");
}

#[test]
fn a_reader_sees_original_and_update_as_one_store() {
    let reg = HandlerRegistry::builtin();
    let bytes =
        fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample1.heic"));
    let Ok(bytes) = bytes else { return };

    let first = c2pa_store(&[b"m1"]);
    let second = c2pa_store(&[b"m1", b"m2"]);

    // Start from a file with no store, whatever the fixture holds.
    let mut none = Vec::new();
    open(&reg, &bytes)
        .unwrap()
        .write(&mut none, &Updates::remove_all(), &mut NullSink)
        .unwrap();
    let s0 = open(&reg, &none).unwrap();
    assert_eq!(s0.c2pa_store().unwrap(), None);

    let mut a = Vec::new();
    s0.write(
        &mut a,
        &Updates::new().set_c2pa(first.clone()),
        &mut NullSink,
    )
    .unwrap();
    let s1 = open(&reg, &a).unwrap();
    assert_eq!(s1.c2pa_store().unwrap(), Some(first));

    let mut b = Vec::new();
    s1.write(
        &mut b,
        &Updates::new().set_c2pa(second.clone()).as_update(),
        &mut NullSink,
    )
    .unwrap();
    let s2 = open(&reg, &b).unwrap();
    // Two stores are in the file, and a reader sees them as the one that was set.
    assert_eq!(s2.items_of(ItemKind::C2pa).count(), 2);
    assert_eq!(s2.c2pa_store().unwrap(), Some(second));
}

/// The bytes of the segments an item lives in, in order.
#[test]
fn compose_rejects_what_it_cannot_encode() {
    let reg = HandlerRegistry::builtin();
    assert!(reg
        .handler("jpg")
        .unwrap()
        .compose(ItemKind::C2pa, b"not a superbox")
        .is_err());
    assert!(reg
        .handler("jpg")
        .unwrap()
        .compose(ItemKind::Exif, b"x")
        .is_err());
    assert!(reg
        .handler("png")
        .unwrap()
        .compose(ItemKind::EmbeddedAsset, b"x")
        .is_err());
    // Kinds a format cannot hold.
    for (format, kind) in [
        ("gif", ItemKind::Exif),
        ("mp3", ItemKind::Exif),
        ("svg", ItemKind::Xmp),
    ] {
        assert!(
            reg.handler(format).unwrap().compose(kind, b"x").is_err(),
            "{format} {kind}"
        );
    }
    assert!(reg.handler("xyz").is_none());
}

#[test]
fn media_types_by_format_string() {
    let reg = HandlerRegistry::builtin();
    for (format, mime) in [
        ("jpg", "image/jpeg"),
        ("JPEG", "image/jpeg"),
        ("mpo", "image/jpeg"),
        ("image/jpeg", "image/jpeg"),
        (" .png ", "image/png"),
        ("heic", "image/heic"),
        ("avif", "image/avif"),
        ("m4a", "audio/mp4"),
        ("mov", "video/quicktime"),
        ("video/mp4", "video/mp4"),
        ("webp", "image/webp"),
        ("wav", "audio/wav"),
        ("avi", "video/avi"),
        ("image/x-webp", "image/webp"),
        ("audio/x-wav", "audio/wav"),
        ("application/mp4", "video/mp4"),
    ] {
        assert_eq!(reg.media_type_for(format), Some(mime), "{format}");
    }
    assert_eq!(reg.media_type_for("xyz"), None);
    for (format, ext) in [
        ("image/jpeg", "jpg"),
        ("jpeg", "jpg"),
        ("mpo", "jpg"),
        ("image/png", "png"),
        ("image/heic", "heic"),
        ("audio/mp4", "m4a"),
        ("audio/wave", "wav"),
    ] {
        assert_eq!(reg.extension_for(format), Some(ext), "{format}");
    }
    assert_eq!(reg.extension_for("xyz"), None);
    assert_eq!(
        reg.container_for("image/jpeg"),
        Some(asset_io::containers::jpeg::ID)
    );
    assert_eq!(
        reg.container_for("mov"),
        Some(asset_io::containers::bmff::ID)
    );
    // Same container family, different media types.
    assert_eq!(reg.container_for("heic"), reg.container_for("mp4"));
    assert_eq!(reg.container_for("wav"), reg.container_for("webp"));

    // A caller's registry built from the built-ins still answers for them.
    let ext = HandlerRegistry::with_builtin();
    assert_eq!(ext.media_type_for("heic"), Some("image/heic"));
}

/// Kinds, encodings and categories agree in every container, for items parsed and
/// for items written.
#[test]
fn item_kinds_encodings_and_categories_per_container() {
    use asset_io::{Encoding, ItemCategory, SegmentId, SegmentKind};
    let check = |s: &Structure, what: &str| {
        for e in s.items() {
            let ok = match e.kind() {
                ItemKind::Xmp => e.encoding() == Encoding::Xml,
                ItemKind::C2pa => e.encoding() == Encoding::Jumbf,
                ItemKind::Exif => e.encoding() == Encoding::Tiff,
                ItemKind::EmbeddedAsset => e.encoding() == Encoding::Asset,
                _ => matches!(e.encoding(), Encoding::IsoBmff | Encoding::Unknown),
            };
            assert!(ok, "{what}: {:?}", container_api::item_info(e));
            let category = match e.kind() {
                ItemKind::EmbeddedAsset => ItemCategory::Asset,
                ItemKind::Other => ItemCategory::Other,
                _ => ItemCategory::Metadata,
            };
            assert_eq!(e.kind().category(), category, "{what}");
        }
    };
    let reg = HandlerRegistry::builtin();
    let xmp = b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>".to_vec();
    let jumbf = c2pa_store(&[b"one"]);
    let mut written = std::collections::BTreeSet::new();
    for (name, bytes) in fixtures() {
        let Ok(s) = open(&reg, &bytes) else {
            continue;
        };
        check(&s, &name);
        let updates = Updates::new().set_xmp(xmp.clone()).set_c2pa(jumbf.clone());
        let mut out = Vec::new();
        let Ok(dest) = s.write(&mut out, &updates, &mut NullSink) else {
            continue;
        };
        let dest = Asset::from_parts(dest, Cursor::new(&out));
        check(&dest, &name);
        // What a write inserts is a metadata item in a metadata segment.
        for (kind, encoding, data) in [
            (ItemKind::Xmp, Encoding::Xml, &xmp),
            (ItemKind::C2pa, Encoding::Jumbf, &jumbf),
        ] {
            let found = dest
                .items_of(kind)
                .find(|e| dest.read_content(e).unwrap() == *data);
            let e = found.unwrap_or_else(|| panic!("{name}: no {kind} written"));
            assert_eq!(e.encoding(), encoding, "{name}");
            assert_eq!(e.kind().category(), ItemCategory::Metadata, "{name}");
            for x in e.extents() {
                let seg = dest.segment(x.segment()).unwrap();
                assert_eq!(seg.kind(), SegmentKind::Metadata, "{name} {kind}");
                assert!(dest.items_in(x.segment()).any(|i| i.kind() == kind));
            }
        }
        let first = SegmentId::from_index(0).unwrap();
        assert_eq!(
            dest.segment(first).unwrap().kind(),
            SegmentKind::Preamble,
            "{name}"
        );
        written.insert(s.container_id().to_owned());
    }
    for id in ["jpeg", "png", "bmff", "riff"] {
        assert!(written.contains(id), "no {id} fixture was written");
    }
}
