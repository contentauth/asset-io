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
    jumbf::C2PA_STORE_TYPE, Asset, Error, HandlerRegistry, ItemKind, NullSink, Structure, Updates,
};

fn boxed(ty: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(ty);
    v.extend_from_slice(body);
    v
}

/// A JUMBF superbox of type `ty` holding one `json` child per entry of `children`.
fn superbox(ty: [u8; 16], children: &[&[u8]]) -> Vec<u8> {
    let mut d = ty.to_vec();
    d.push(3);
    d.extend_from_slice(b"c2pa\0");
    let mut body = boxed(b"jumd", &d);
    for c in children {
        body.extend_from_slice(&boxed(b"json", c));
    }
    boxed(b"jumb", &body)
}

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

#[test]
fn jpeg_store_is_found_beside_other_jumbf() {
    let other = superbox([1; 16], &[b"not c2pa"]);
    let c2pa = superbox(C2PA_STORE_TYPE, &[b"manifest"]);
    let with_other = write(
        "jpg",
        &[0xff, 0xd8, 0xff, 0xd9],
        &Updates::new().set_jumbf(other),
    );

    let asset = open("jpg", &with_other);
    assert!(asset.c2pa_item().unwrap().is_none());
    assert_eq!(asset.c2pa_store().unwrap(), None);

    // A store of another type is kept when a C2PA store is added.
    let both = write("jpg", &with_other, &Updates::new().set_jumbf(c2pa.clone()));
    let asset = open("jpg", &both);
    assert_eq!(asset.items_of(ItemKind::Jumbf).count(), 2);
    assert!(matches!(asset.store(), Err(Error::Ambiguous { .. })));
    let item = asset.c2pa_item().unwrap().unwrap();
    assert_eq!(item.jumbf_type(), Some(C2PA_STORE_TYPE));
    assert_eq!(asset.c2pa_store().unwrap(), Some(c2pa));
}

#[test]
fn svg_store_is_found_through_its_base64_transfer_encoding() {
    let c2pa = superbox(C2PA_STORE_TYPE, &[b"manifest"]);
    let svg = write(
        "svg",
        br#"<svg xmlns="http://www.w3.org/2000/svg"></svg>"#,
        &Updates::new().set_jumbf(c2pa.clone()),
    );
    let asset = open("svg", &svg);
    assert!(asset.c2pa_item().unwrap().is_some());
    assert_eq!(asset.c2pa_store().unwrap(), Some(c2pa));
}

#[test]
fn bmff_original_and_update_read_as_one_c2pa_store() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample1.heic");
    let Ok(fixture) = fs::read(path) else { return };
    let first = superbox(C2PA_STORE_TYPE, &[b"m1"]);
    let second = superbox(C2PA_STORE_TYPE, &[b"m1", b"m2"]);
    let none = write("heic", &fixture, &Updates::remove_all());
    let original = write("heic", &none, &Updates::new().set_jumbf(first.clone()));
    let updated = write(
        "heic",
        &original,
        &Updates::new().set_jumbf(second.clone()).as_update(),
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
    let c2pa = superbox(C2PA_STORE_TYPE, &[b"manifest"]);
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
        let signed = write(format, &input, &Updates::new().set_jumbf(c2pa.clone()));
        for (source, updates) in [
            (&input, Updates::new().set_jumbf(c2pa.clone())),
            // An existing store that is kept, or for BMFF, relabelled as the
            // original of an update, takes its type from the parsed source.
            (&signed, Updates::new()),
        ] {
            let planned = open(format, source).plan(&updates).unwrap();
            let item = planned.c2pa_item(&mut Cursor::new([])).unwrap();
            assert!(item.is_some(), "{format}");
        }
        if format == "heic" {
            let update = superbox(C2PA_STORE_TYPE, &[b"manifest", b"more"]);
            let planned = open(format, &signed)
                .plan(&Updates::new().set_jumbf(update).as_update())
                .unwrap();
            let item = planned.c2pa_item(&mut Cursor::new([])).unwrap().unwrap();
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
/// the I/O, as the blocking calls do. Recorded JUMBF types mean finding the item
/// reads nothing.
#[test]
fn one_task_finds_the_store_as_the_blocking_calls_do() {
    let registry = HandlerRegistry::builtin();
    let c2pa = superbox(C2PA_STORE_TYPE, &[b"manifest"]);
    let mut assets = vec![
        (
            "jpg",
            write(
                "jpg",
                &[0xff, 0xd8, 0xff, 0xd9],
                &Updates::new().set_jumbf(c2pa.clone()),
            ),
        ),
        ("jpg", vec![0xff, 0xd8, 0xff, 0xd9]),
        (
            "svg",
            write(
                "svg",
                br#"<svg xmlns="http://www.w3.org/2000/svg"></svg>"#,
                &Updates::new().set_jumbf(c2pa.clone()),
            ),
        ),
    ];
    let heic = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample1.heic");
    if let Ok(fixture) = fs::read(heic) {
        let none = write("heic", &fixture, &Updates::remove_all());
        let original = write("heic", &none, &Updates::new().set_jumbf(c2pa.clone()));
        let update = superbox(C2PA_STORE_TYPE, &[b"manifest", b"more"]);
        let updated = write(
            "heic",
            &original,
            &Updates::new().set_jumbf(update).as_update(),
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
            let boxes = structure.c2pa_boxes_async(&mut io).await.ok();
            let store = structure.c2pa_store_async(&mut io).await?;
            Ok((store, boxes))
        });
        let (got, _) = drive(task, bytes);
        let (store, boxes) = got.unwrap();
        assert_eq!(store, want_store, "{format}");
        assert_eq!(boxes, want_boxes, "{format}");

        let task = asset_io::task::Task::new(bytes.len() as u64, |mut io| async move {
            structure
                .c2pa_item_async(&mut io)
                .await
                .map(|i| i.is_some())
        });
        let (found, requests) = drive(task, bytes);
        assert_eq!(found.unwrap(), want_store.is_some(), "{format}");
        if *format != "svg" {
            assert_eq!(requests, 0, "{format}: the item type was recorded");
        }
    }
}

#[test]
fn boxes_need_the_store_item_of_the_same_structure() {
    let c2pa = superbox(C2PA_STORE_TYPE, &[b"manifest"]);
    let bytes = write(
        "jpg",
        &[0xff, 0xd8, 0xff, 0xd9],
        &Updates::new().set_jumbf(c2pa),
    );
    let (a, b) = (open("jpg", &bytes), open("jpg", &bytes));
    let item = b.c2pa_item().unwrap().unwrap();
    assert!(matches!(
        a.boxes_with(item, "store"),
        Err(Error::InvalidFormat(_))
    ));
}
