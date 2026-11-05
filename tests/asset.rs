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

//! `Asset`: a parsed structure together with the blocking stream it describes.
#![cfg(all(feature = "jpeg", feature = "png", feature = "svg"))]

use std::{io::Cursor, sync::Arc};

use asset_io::{
    jumbf::C2PA_STORE_TYPE, Asset, Chunk, Error, HandlerRegistry, ItemKind, NullSink, Structure,
    Updates,
};

const JPEG: &[u8] = &[0xff, 0xd8, 0xff, 0xd9];
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\0IEND\xae\x42\x60\x82";
const SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg"></svg>"#;

fn boxed(ty: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(ty);
    v.extend_from_slice(body);
    v
}

/// A C2PA manifest store holding one `json` child.
fn c2pa_store(child: &[u8]) -> Vec<u8> {
    let mut d = C2PA_STORE_TYPE.to_vec();
    d.push(3);
    d.extend_from_slice(b"c2pa\0");
    let mut body = boxed(b"jumd", &d);
    body.extend_from_slice(&boxed(b"json", child));
    boxed(b"jumb", &body)
}

#[test]
fn reads_match_the_structure_api_and_geometry_is_available_directly() {
    let registry = HandlerRegistry::builtin();
    let xmp = br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"/>"#.to_vec();
    let mut bytes = Vec::new();
    Asset::open(Cursor::new(PNG), Some("png"))
        .unwrap()
        .write(
            &mut bytes,
            &Updates::new().set_xmp(xmp.clone()),
            &mut NullSink,
        )
        .unwrap();

    let asset = Asset::open(Cursor::new(&bytes), None).unwrap();
    let mut src = Cursor::new(&bytes);
    let structure = Structure::parse(&registry, &mut src, None).unwrap();

    // Geometry comes from the structure through Deref.
    assert_eq!(asset.container_id(), "png");
    assert_eq!(asset.total_size(), bytes.len() as u64);
    assert_eq!(asset.segments(), structure.segments());
    let item = asset.require_item(ItemKind::Xmp).unwrap();

    assert_eq!(asset.xmp().unwrap(), Some(xmp.clone()));
    assert_eq!(asset.read_content(item).unwrap(), xmp);
    assert_eq!(
        asset.read_item(item).unwrap(),
        structure
            .read_item(&mut src, structure.require_item(ItemKind::Xmp).unwrap())
            .unwrap()
    );
    let first = item.extents().next().unwrap().range();
    assert_eq!(
        asset.read_range(first).unwrap(),
        &bytes[first.offset() as usize..first.end() as usize]
    );
    let mut seen = Vec::new();
    asset
        .read(&mut |chunk: Chunk<'_>| {
            seen.extend_from_slice(chunk.data);
            Ok(())
        })
        .unwrap();
    assert_eq!(seen, bytes);
    let mut nodes = 0;
    asset
        .walk(|_| {
            nodes += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(nodes, asset.segments().len());
    assert_eq!(
        asset.plan(&Updates::new()).unwrap().total_size(),
        bytes.len() as u64
    );
}

/// The C2PA embedding flow: write a placeholder store, find it in the output's
/// structure without parsing the output again, and fill it in place.
#[test]
fn a_placeholder_is_found_and_filled_in_the_written_output() {
    for (format, input) in [("jpg", JPEG), ("png", PNG), ("svg", SVG)] {
        let placeholder = c2pa_store(&[0; 64]);
        let signed = c2pa_store(&[7; 64]);
        assert_eq!(placeholder.len(), signed.len());

        let asset = Asset::open(Cursor::new(input), Some(format)).unwrap();
        let mut out = Cursor::new(Vec::new());
        let written = asset
            .write(
                &mut out,
                &Updates::new().set_jumbf(placeholder.clone()),
                &mut NullSink,
            )
            .unwrap();

        let output = Asset::from_parts(written, out);
        let item = output.c2pa_item().unwrap().unwrap();
        assert!(!output.c2pa_data_hash_exclusions(item).unwrap().is_empty());
        assert_eq!(output.read_content(item).unwrap(), placeholder);
        // Three bytes short changes the stored length even through SVG's base64.
        assert!(
            matches!(
                output.patch_exact(item, &signed[3..]),
                Err(Error::InvalidFormat(_))
            ),
            "{format}"
        );
        output.patch_exact(item, &signed).unwrap();

        let bytes = output.into_source().into_inner();
        let reparsed = Asset::open(Cursor::new(&bytes), Some(format)).unwrap();
        assert_eq!(reparsed.c2pa_store().unwrap(), Some(signed), "{format}");
    }
}

#[test]
fn borrowed_and_owned_sources_behave_the_same() {
    let mut file = Cursor::new(JPEG.to_vec());
    {
        let mut borrowed = Asset::open(&mut file, None).unwrap();
        assert_eq!(borrowed.container_id(), "jpeg");
        assert_eq!(borrowed.store().unwrap(), None);
        borrowed.source_mut().set_position(0);
    }
    // The borrow has ended; the caller still owns the stream.
    file.set_position(2);
    let owned = Asset::open(file, None).unwrap();
    assert_eq!(owned.total_size(), JPEG.len() as u64);
    let (structure, source) = owned.into_parts();
    assert_eq!(structure.total_size(), source.get_ref().len() as u64);
}

#[test]
fn opening_reports_format_selection_and_parse_errors() {
    assert_eq!(
        Asset::open(Cursor::new(JPEG), Some("unknown"))
            .unwrap()
            .container_id(),
        "jpeg"
    );
    assert_eq!(
        Asset::open(Cursor::new(JPEG), Some("png"))
            .unwrap()
            .container_id(),
        "jpeg"
    );
    // SVG has no detector: it needs an explicit format.
    assert!(matches!(
        Asset::open(Cursor::new(SVG), None),
        Err(Error::UnsupportedFormat)
    ));
    assert!(Asset::open(Cursor::new(SVG), Some("svg")).is_ok());
    assert!(Asset::open(Cursor::new(&JPEG[..2]), Some("jpg")).is_err());
}

/// `open` uses the built-in handlers; `open_with` uses exactly the registry given.
#[test]
fn open_uses_the_builtins_and_open_with_the_given_registry() {
    let builtin = HandlerRegistry::builtin();
    let opened = Asset::open(Cursor::new(PNG), None).unwrap();
    let with = Asset::open_with(&builtin, Cursor::new(PNG), None).unwrap();
    assert_eq!(opened.segments(), with.segments());
    assert_eq!(opened.container_id(), "png");

    let empty = HandlerRegistry::new();
    assert!(matches!(
        Asset::open_with(&empty, Cursor::new(PNG), None),
        Err(Error::UnsupportedFormat)
    ));
    let only_jpeg =
        HandlerRegistry::new().with_handlers([Arc::clone(builtin.handler("jpg").unwrap())]);
    assert!(Asset::open_with(&only_jpeg, Cursor::new(JPEG), None).is_ok());
    assert!(matches!(
        Asset::open_with(&only_jpeg, Cursor::new(PNG), Some("png")),
        Err(Error::UnsupportedFormat)
    ));
}

#[test]
fn calling_back_into_the_asset_from_a_sink_is_an_error_not_a_panic() {
    let asset = Asset::open(Cursor::new(JPEG), None).unwrap();
    let mut inner = None;
    let result = asset.read(&mut |_: Chunk<'_>| {
        inner.get_or_insert_with(|| asset.store().map(|_| ()));
        Ok(())
    });
    result.unwrap();
    assert!(matches!(inner, Some(Err(Error::Io(_)))));
    // The outer call has finished, so the asset is usable again.
    assert_eq!(asset.store().unwrap(), None);
}
