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

#![no_main]

//! Multi-Picture Format: the fuzzer's bytes are the MPF index of an otherwise valid
//! two-image JPEG, followed by the trailing data. Parsing and copying must always
//! work; an edit may be refused, but one that is accepted must write a file that
//! parses to the structure `write` reported.

use std::io::Cursor;

use asset_io::{HandlerRegistry, NullSink, Structure, Updates};
use libfuzzer_sys::fuzz_target;

/// The largest block an APP2 segment can hold after `MPF\0`.
const MAX_BLOCK: usize = 60_000;

fn marker(m: u8, data: &[u8]) -> Vec<u8> {
    let mut v = vec![0xff, m];
    v.extend_from_slice(&((data.len() + 2) as u16).to_be_bytes());
    v.extend_from_slice(data);
    v
}

/// A small complete JPEG, with the MPF block in an APP2 segment when given.
fn image(mpf: Option<&[u8]>) -> Vec<u8> {
    let mut v = vec![0xff, 0xd8];
    v.extend(marker(0xe0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0"));
    if let Some(block) = mpf {
        let mut p = b"MPF\0".to_vec();
        p.extend_from_slice(block);
        v.extend(marker(0xe2, &p));
    }
    v.extend(marker(0xdb, &[0u8; 65]));
    v.extend(marker(0xda, &[1, 1, 0, 0, 0x3f, 0]));
    v.extend_from_slice(&[0x12, 0x34, 0xff, 0x00, 0x56, 0xff, 0xd9]);
    v
}

fuzz_target!(|data: &[u8]| {
    let (block, tail) = data.split_at(data.len() * 3 / 4);
    let block = &block[..block.len().min(MAX_BLOCK)];
    let mut file = image(Some(block));
    file.extend(image(None));
    file.extend_from_slice(tail);

    let registry = HandlerRegistry::builtin();
    let Ok(structure) = Structure::parse(&registry, &mut Cursor::new(&file), Some("jpg")) else {
        return;
    };

    let cases = [
        Updates::new(),
        Updates::new().set_c2pa(asset_io::testing::c2pa_store(&[b"manifest"])),
        Updates::new().set_xmp(b"<x:xmpmeta/>".to_vec()),
        Updates::remove_all(),
    ];
    for updates in &cases {
        let mut out = Cursor::new(Vec::new());
        let Ok(dest) = structure.write(&mut Cursor::new(&file), &mut out, updates, &mut NullSink)
        else {
            continue;
        };
        let out = out.into_inner();
        assert_eq!(dest.total_size(), out.len() as u64);
        let again = Structure::parse(&registry, &mut Cursor::new(&out), Some("jpg"))
            .expect("the output of an accepted edit parses");
        assert_eq!(again.subassets(), dest.subassets());
    }
});
