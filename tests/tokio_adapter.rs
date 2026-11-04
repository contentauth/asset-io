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

#![cfg(all(feature = "tokio", feature = "jpeg", feature = "png", feature = "svg"))]

use std::io::Cursor;

use asset_io::{
    task::Task, tokio as asset, Asset, Chunk, Error, HandlerRegistry, ItemKind, NullSink, Updates,
};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

fn inputs() -> [(&'static str, Vec<u8>); 3] {
    [
        ("jpg", vec![0xff, 0xd8, 0xff, 0xd9]),
        (
            "png",
            b"\x89PNG\r\n\x1a\n\0\0\0\0IEND\xae\x42\x60\x82".to_vec(),
        ),
        (
            "svg",
            br#"<svg xmlns="http://www.w3.org/2000/svg"></svg>"#.to_vec(),
        ),
    ]
}

fn store() -> Vec<u8> {
    let mut bytes = 33u32.to_be_bytes().to_vec();
    bytes.extend_from_slice(b"jumb");
    bytes.extend_from_slice(&25u32.to_be_bytes());
    bytes.extend_from_slice(b"jumd");
    bytes.extend_from_slice(&asset_io::jumbf::C2PA_STORE_TYPE);
    bytes.push(0);
    bytes
}

#[tokio::test(flavor = "multi_thread")]
async fn spawned_adapters_match_blocking_reads_writes_and_sink_bytes() {
    for (format, input) in inputs() {
        tokio::spawn(async move {
            let registry = HandlerRegistry::builtin();
            let updates = Updates::new().set_jumbf(store());
            let blocking = Asset::open(Cursor::new(input.clone()), Some(format)).unwrap();
            let mut expected = Cursor::new(Vec::new());
            let expected_layout = blocking
                .write(&mut expected, &updates, &mut NullSink)
                .unwrap();

            // Parsing is independent of the stream's initial position.
            let mut src = Cursor::new(input);
            src.set_position(1);
            let structure = asset::parse(&registry, &mut src, Some(format))
                .await
                .unwrap();
            assert_eq!(structure.total_size(), blocking.total_size());
            let plan = asset::write_plan(&structure, &mut src, &updates)
                .await
                .unwrap();
            let mut output = Cursor::new(Vec::new());
            let mut seen = Vec::new();
            let mut sink = |chunk: Chunk<'_>| {
                seen.extend_from_slice(chunk.data);
                Ok(())
            };
            asset::execute(&plan, &mut src, &mut output, &mut sink)
                .await
                .unwrap();
            assert_eq!(output.get_ref(), expected.get_ref(), "{format}");
            assert_eq!(seen, *expected.get_ref(), "{format}: sink");
            assert_eq!(plan.structure().total_size(), expected_layout.total_size());

            let parsed = if format == "svg" {
                // SVG has no built-in content detector.
                asset::parse(&registry, &mut output, Some(format))
                    .await
                    .unwrap()
            } else {
                asset::parse(&registry, &mut output, None).await.unwrap()
            };
            let item = asset::c2pa_item(&parsed, &mut output)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(item.kind(), ItemKind::Jumbf);
            assert_eq!(
                asset::read_item(&parsed, &mut output, item).await.unwrap(),
                parsed
                    .read_item(&mut Cursor::new(output.get_ref()), item)
                    .unwrap()
            );
            assert_eq!(
                asset::read_content(&parsed, &mut output, item)
                    .await
                    .unwrap(),
                store()
            );
            assert_eq!(
                asset::c2pa_store(&parsed, &mut output).await.unwrap(),
                Some(store())
            );
            let bytes = asset::run(
                Task::new(parsed.total_size(), |mut io| async move {
                    let item = parsed.c2pa_item_async(&mut io).await?.unwrap();
                    parsed.read_content_async(&mut io, item).await
                }),
                &mut output,
            )
            .await
            .unwrap();
            assert_eq!(bytes, store());

            let mut written = Cursor::new(Vec::new());
            let layout = asset::write(&structure, &mut src, &mut written, &updates, &mut NullSink)
                .await
                .unwrap();
            assert_eq!(written.get_ref(), expected.get_ref());
            assert_eq!(layout.total_size(), expected_layout.total_size());
        })
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn writes_respect_output_position_and_do_not_truncate() {
    let registry = HandlerRegistry::builtin();
    let mut src = Cursor::new(inputs()[0].1.clone());
    let structure = asset::parse(&registry, &mut src, None).await.unwrap();
    let mut output = Cursor::new(b"prefix........tail".to_vec());
    output.seek(std::io::SeekFrom::Start(6)).await.unwrap();
    let written = asset::write(
        &structure,
        &mut src,
        &mut output,
        &Updates::new(),
        &mut NullSink,
    )
    .await
    .unwrap();
    assert_eq!(written.total_size(), 4);
    assert_eq!(&output.get_ref()[..6], b"prefix");
    assert_eq!(&output.get_ref()[6..10], &[0xff, 0xd8, 0xff, 0xd9]);
    assert_eq!(&output.get_ref()[10..], b"....tail");
}

#[tokio::test]
async fn adapters_propagate_parse_read_and_sink_errors() {
    let registry = HandlerRegistry::builtin();
    assert!(matches!(
        asset::parse(
            &registry,
            &mut Cursor::new(Vec::<u8>::new()),
            Some("unknown")
        )
        .await,
        Err(Error::UnsupportedFormat)
    ));
    assert!(asset::parse(&registry, &mut Cursor::new(b"invalid"), None)
        .await
        .is_err());

    let mut src = Cursor::new(inputs()[0].1.clone());
    let structure = asset::parse(&registry, &mut src, None).await.unwrap();
    assert!(asset::c2pa_item(&structure, &mut src)
        .await
        .unwrap()
        .is_none());
    assert!(asset::c2pa_store(&structure, &mut src)
        .await
        .unwrap()
        .is_none());
    let updates = Updates::new().set_jumbf(store());
    let mut signed = Cursor::new(Vec::new());
    asset::write(&structure, &mut src, &mut signed, &updates, &mut NullSink)
        .await
        .unwrap();
    let parsed = asset::parse(&registry, &mut signed, None).await.unwrap();
    let item = asset::c2pa_item(&parsed, &mut signed)
        .await
        .unwrap()
        .unwrap();
    let mut empty = Cursor::new(Vec::<u8>::new());
    assert!(matches!(
        asset::read_item(&parsed, &mut empty, item).await,
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof
    ));
    let unchanged = Updates::new();
    let plan = asset::write_plan(&structure, &mut src, &unchanged)
        .await
        .unwrap();
    let mut output = Vec::new();
    assert!(matches!(
        asset::execute(&plan, &mut empty, &mut output, &mut NullSink).await,
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof
    ));
    let mut cancel = |_: Chunk<'_>| Err(Error::UserCanceled);
    assert!(matches!(
        asset::execute(&plan, &mut src, &mut output, &mut cancel).await,
        Err(Error::UserCanceled)
    ));

    // A closed async destination reports its write error to the caller.
    let (mut writer, reader) = tokio::io::duplex(64);
    drop(reader);
    assert!(matches!(
        asset::execute(&plan, &mut src, &mut writer, &mut NullSink).await,
        Err(Error::Io(_))
    ));
    writer.shutdown().await.unwrap();
}
