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

//! Finding the C2PA manifest store among whatever JUMBF an asset holds.
#![cfg(all(
    feature = "jpeg",
    feature = "png",
    feature = "riff",
    feature = "svg",
    feature = "bmff"
))]

use std::{fs, io::Cursor, path::PathBuf};

use asset_io::{
    jumbf::C2PA_STORE_TYPE,
    testing::{c2pa_store, jumbf},
    Asset, Error, HandlerRegistry, ItemKind, NullSink, Structure, Updates,
};

fn open<'a>(format: &str, bytes: &'a [u8]) -> Asset<Cursor<&'a [u8]>> {
    Asset::open(Cursor::new(bytes), Some(format)).unwrap()
}

fn write(format: &str, input: &[u8], updates: &Updates) -> Vec<u8> {
    let mut out = Vec::new();
    open(format, input)
        .write(&mut out, updates, &mut NullSink)
        .unwrap();
    out
}

/// A JPEG holding `jumbf` in one JPEG XT APP11 segment.
fn jpeg_with_jumbf(jumbf: &[u8]) -> Vec<u8> {
    let mut app11 = b"JP".to_vec();
    app11.extend_from_slice(&7u16.to_be_bytes());
    app11.extend_from_slice(&1u32.to_be_bytes());
    app11.extend_from_slice(jumbf);
    let mut jpeg = vec![0xff, 0xd8, 0xff, 0xeb];
    jpeg.extend_from_slice(&u16::try_from(app11.len() + 2).unwrap().to_be_bytes());
    jpeg.extend(app11);
    jpeg.extend_from_slice(&[0xff, 0xd9]);
    jpeg
}

#[test]
fn jpeg_store_is_found_beside_other_jumbf() {
    let other = jumbf([1; 16], &[b"not c2pa"]);
    let c2pa = c2pa_store(&[b"manifest"]);
    let with_other = jpeg_with_jumbf(&other);

    let asset = open("jpg", &with_other);
    assert!(asset.c2pa_item().unwrap().is_none());
    assert_eq!(asset.c2pa_store().unwrap(), None);
    let item = asset.require_item(ItemKind::Other).unwrap();
    assert_eq!(item.encoding(), asset_io::Encoding::Jumbf);
    assert_eq!(item.jumbf_type(), Some([1; 16]));

    // Other JUMBF is kept when a C2PA store is added, and when it is removed.
    let both = write("jpg", &with_other, &Updates::new().set_c2pa(c2pa.clone()));
    let asset = open("jpg", &both);
    assert_eq!(asset.items_of(ItemKind::C2pa).count(), 1);
    let item = asset.c2pa_item().unwrap().unwrap();
    assert_eq!(item.jumbf_type(), Some(C2PA_STORE_TYPE));
    assert_eq!(asset.c2pa_store().unwrap(), Some(c2pa));
    let other_item = asset.require_item(ItemKind::Other).unwrap();
    assert_eq!(asset.read_item(other_item).unwrap(), other);

    let removed = write("jpg", &both, &Updates::new().remove_c2pa());
    assert_eq!(removed, with_other);
}

#[test]
fn a_jpeg_store_must_have_the_c2pa_type() {
    let asset = open("jpg", &[0xff, 0xd8, 0xff, 0xd9]);
    let updates = Updates::new().set_c2pa(jumbf([1; 16], &[b"not c2pa"]));
    assert!(matches!(
        asset.write(&mut Vec::new(), &updates, &mut NullSink),
        Err(Error::InvalidFormat(_))
    ));
}

#[test]
fn svg_store_is_found_through_its_base64_transfer_encoding() {
    let c2pa = c2pa_store(&[b"manifest"]);
    let svg = write(
        "svg",
        br#"<svg xmlns="http://www.w3.org/2000/svg"></svg>"#,
        &Updates::new().set_c2pa(c2pa.clone()),
    );
    let asset = open("svg", &svg);
    assert!(asset.c2pa_item().unwrap().is_some());
    assert_eq!(asset.c2pa_store().unwrap(), Some(c2pa));
}

#[test]
fn bmff_original_and_update_read_as_one_c2pa_store() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample1.heic");
    let Ok(fixture) = fs::read(path) else { return };
    let first = c2pa_store(&[b"m1"]);
    let second = c2pa_store(&[b"m1", b"m2"]);
    let none = write("heic", &fixture, &Updates::remove_all());
    let original = write("heic", &none, &Updates::new().set_c2pa(first.clone()));
    let updated = write(
        "heic",
        &original,
        &Updates::new().set_c2pa(second.clone()).as_update(),
    );

    for (bytes, expected) in [
        (&none, None),
        (&original, Some(first)),
        (&updated, Some(second)),
    ] {
        assert_eq!(open("heic", bytes).c2pa_store().unwrap(), expected);
    }
    let asset = open("heic", &updated);
    let item = asset.c2pa_item().unwrap().unwrap();
    assert_eq!(item.label(), Some(asset_io::labels::ORIGINAL));
}

/// A planned structure describes output that does not exist yet, so the C2PA store
/// must be identifiable without reading it: a reader with no bytes is enough.
#[test]
fn planned_stores_are_identified_without_reading_the_output() {
    let c2pa = c2pa_store(&[b"manifest"]);
    let mut inputs = vec![
        ("jpg", vec![0xff, 0xd8, 0xff, 0xd9]),
        (
            "png",
            b"\x89PNG\r\n\x1a\n\0\0\0\0IEND\xae\x42\x60\x82".to_vec(),
        ),
        ("wav", b"RIFF\x04\0\0\0WAVE".to_vec()),
        (
            "svg",
            br#"<svg xmlns="http://www.w3.org/2000/svg"></svg>"#.to_vec(),
        ),
    ];
    let heic = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample1.heic");
    if let Ok(fixture) = fs::read(heic) {
        inputs.push(("heic", write("heic", &fixture, &Updates::remove_all())));
    }
    for (format, input) in inputs {
        let signed = write(format, &input, &Updates::new().set_c2pa(c2pa.clone()));
        for (source, updates) in [
            (&input, Updates::new().set_c2pa(c2pa.clone())),
            // An existing store that is kept, or for BMFF, relabelled as the
            // original of an update, takes its type from the parsed source.
            (&signed, Updates::new()),
        ] {
            let planned = open(format, source).plan(&updates).unwrap();
            assert!(planned.c2pa_item().unwrap().is_some(), "{format}");
        }
        if format == "heic" {
            let update = c2pa_store(&[b"manifest", b"more"]);
            let planned = open(format, &signed)
                .plan(&Updates::new().set_c2pa(update).as_update())
                .unwrap();
            let item = planned.c2pa_item().unwrap().unwrap();
            assert_eq!(item.label(), Some(asset_io::labels::ORIGINAL));
        }
    }
}

/// Run `task` against `source`, answering each request from the slice. Returns the
/// result and how many requests were made.
fn drive<T>(mut task: asset_io::task::Task<'_, T>, source: &[u8]) -> (asset_io::Result<T>, usize) {
    let mut requests = 0;
    loop {
        match task.resume() {
            Ok(asset_io::task::Progress::Read(mut req)) => {
                requests += 1;
                let from = (req.offset() as usize).min(source.len());
                let to = (from + req.window_len()).min(source.len());
                req.buffer().extend_from_slice(&source[from..to]);
            }
            Ok(asset_io::task::Progress::Done(v)) => return (Ok(v), requests),
            Ok(_) => unreachable!("a progress this driver does not know"),
            Err(e) => return (Err(e), requests),
        }
    }
}

/// One task parses the asset and finds its store and boxes, with the caller doing
/// the I/O, as the blocking calls do. Finding the item needs no I/O at all.
#[test]
fn one_task_finds_the_store_as_the_blocking_calls_do() {
    let registry = HandlerRegistry::builtin();
    let c2pa = c2pa_store(&[b"manifest"]);
    let mut assets = vec![
        (
            "jpg",
            write(
                "jpg",
                &[0xff, 0xd8, 0xff, 0xd9],
                &Updates::new().set_c2pa(c2pa.clone()),
            ),
        ),
        ("jpg", vec![0xff, 0xd8, 0xff, 0xd9]),
        (
            "svg",
            write(
                "svg",
                br#"<svg xmlns="http://www.w3.org/2000/svg"></svg>"#,
                &Updates::new().set_c2pa(c2pa.clone()),
            ),
        ),
    ];
    let heic = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample1.heic");
    if let Ok(fixture) = fs::read(heic) {
        let none = write("heic", &fixture, &Updates::remove_all());
        let original = write("heic", &none, &Updates::new().set_c2pa(c2pa.clone()));
        let update = c2pa_store(&[b"manifest", b"more"]);
        let updated = write(
            "heic",
            &original,
            &Updates::new().set_c2pa(update).as_update(),
        );
        assets.push(("heic", updated));
    }
    for (format, bytes) in &assets {
        let asset = open(format, bytes);
        let want_store = asset.c2pa_store().unwrap();
        let want_boxes = asset.c2pa_boxes().ok();
        let structure = asset.structure();

        let registry = &registry;
        let task = asset_io::task::Task::new(bytes.len() as u64, |mut io| async move {
            let structure = Structure::parse_async(registry, &mut io, Some(format)).await?;
            let boxes = structure.c2pa_boxes().ok();
            let store = structure.c2pa_store_async(&mut io).await?;
            Ok((store, boxes))
        });
        let (got, _) = drive(task, bytes);
        let (store, boxes) = got.unwrap();
        assert_eq!(store, want_store, "{format}");
        assert_eq!(boxes, want_boxes, "{format}");

        assert_eq!(
            structure.c2pa_item().unwrap().is_some(),
            want_store.is_some(),
            "{format}"
        );
    }
}

#[test]
fn boxes_need_the_store_item_of_the_same_structure() {
    let c2pa = c2pa_store(&[b"manifest"]);
    let bytes = write(
        "jpg",
        &[0xff, 0xd8, 0xff, 0xd9],
        &Updates::new().set_c2pa(c2pa),
    );
    let (a, b) = (open("jpg", &bytes), open("jpg", &bytes));
    let item = b.c2pa_item().unwrap().unwrap();
    assert!(matches!(
        a.boxes_with(item, "store"),
        Err(Error::InvalidFormat(_))
    ));
}
