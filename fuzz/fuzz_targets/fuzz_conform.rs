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

use std::io::Cursor;

use asset_io::container_api;
use asset_io::{testing::Conformance, HandlerRegistry, Structure};
use libfuzzer_sys::fuzz_target;

/// A JUMBF with a description box, so a format that wants a type has one.
fn jumbf() -> Vec<u8> {
    let mut v = vec![0x5au8; 200];
    v[0..4].copy_from_slice(&200u32.to_be_bytes());
    v[4..8].copy_from_slice(b"jumb");
    v[8..12].copy_from_slice(&192u32.to_be_bytes());
    v[12..16].copy_from_slice(b"jumd");
    v
}

// Whatever parses must keep the promises every handler makes: the structure tiles
// the stream, a write that changes nothing is byte-identical, plan, write and
// stepper agree, the sink sees every byte once, and what was set reads back.
fuzz_target!(|data: &[u8]| {
    let registry = HandlerRegistry::builtin();
    let mut src = Cursor::new(data);
    let structure = match Structure::parse(&registry, &mut src, None) {
        Ok(s) => s,
        Err(_) => match Structure::parse(&registry, &mut src, Some("svg")) {
            Ok(s) => s,
            Err(_) => return,
        },
    };
    Conformance::new(container_api::handler(&structure).clone())
        .jumbf(Some(jumbf()))
        .mutations(false)
        .check_if_parses("fuzz input", data);
});
