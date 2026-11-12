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

//! Conformance checks for [`ContainerHandler`]s.
//!
//! Every handler has to keep the same promises, whatever its format: the
//! structure it parses tiles the stream, segment by segment and element by
//! element, a write that changes nothing changes
//! nothing, the plan, the write and the stepper agree, the sink sees every output
//! byte once and in order, a C2PA placeholder is filled in place without moving a
//! hashed byte, [`compose`](ContainerHandler::compose) gives what a write inserts,
//! and bad input is an error, never a panic.
//! [`Conformance`] checks all of that on samples you give it, so a handler's own
//! tests only have to cover what is particular to its format.
//!
//! ```no_run
//! use asset_io::testing::Conformance;
//! # use asset_io::container_api::ContainerHandler;
//!
//! # fn check(handler: std::sync::Arc<dyn ContainerHandler>, sample: &[u8]) {
//! Conformance::new(handler).check("sample", sample);
//! # }
//! ```
//!
//! A check panics with the name of the sample and what was wrong, so it can be
//! used directly in a `#[test]`; [`try_check`](Conformance::try_check) returns the
//! description instead.
//!
//! Test data: [`c2pa_store`] and [`c2pa_store_of_len`] build C2PA manifest stores
//! and [`jumbf`] builds JUMBF of another type, so tests need not lay out JUMBF
//! boxes themselves. [`rewrite`] writes a sample with updates in one call.

use std::{
    io::Cursor,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::Arc,
};

use crate::{
    error::{Error, Result},
    execute::Action,
    handler::ContainerHandler,
    item::ItemKind,
    plan::{validate, WritePlan},
    segment::{Role, SegmentKind},
    sink::{Chunk, Sink},
    structure::Structure,
    task::{Progress, Task},
    updates::Updates,
};

/// Compression helpers for synthetic container fixtures.
#[cfg(feature = "flate")]
pub mod compress {
    use std::io::Write;

    use flate2::{
        write::{DeflateEncoder, ZlibEncoder},
        Compression,
    };

    /// Encode a fixture as a zlib stream.
    pub fn zlib(data: &[u8]) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    /// Encode a fixture as raw deflate.
    pub fn raw_deflate(data: &[u8]) -> Vec<u8> {
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }
}

/// A Brotli fixture in one uncompressed meta-block.
///
/// Panics unless `data` contains between 1 and 65536 bytes.
pub fn brotli_stored(data: &[u8]) -> Vec<u8> {
    assert!((1..=65536).contains(&data.len()));
    let mlen = (data.len() - 1) as u32;
    // WBITS=16, ISLAST=0, MNIBBLES=4, MLEN-1, ISUNCOMPRESSED=1.
    let bits = (mlen << 4) | (1 << 20);
    let mut out = bits.to_le_bytes()[..3].to_vec();
    out.extend_from_slice(data);
    out.push(0x03); // ISLAST=1, ISLASTEMPTY=1
    out
}

fn jumbf_box(ty: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = u32::try_from(body.len() + 8)
        .expect("a test box under 4 GiB")
        .to_be_bytes()
        .to_vec();
    v.extend_from_slice(ty);
    v.extend_from_slice(body);
    v
}

fn superbox(jumbf_type: [u8; 16], label: Option<&str>, children: &[&[u8]]) -> Vec<u8> {
    let mut description = jumbf_type.to_vec();
    match label {
        Some(label) => {
            description.push(0x03); // requestable, labelled
            description.extend_from_slice(label.as_bytes());
            description.push(0);
        }
        None => description.push(0),
    }
    let mut body = jumbf_box(b"jumd", &description);
    for child in children {
        body.extend_from_slice(&jumbf_box(b"json", child));
    }
    jumbf_box(b"jumb", &body)
}

/// A C2PA manifest store for tests: a JUMBF superbox of the
/// [C2PA store type](crate::jumbf::C2PA_STORE_TYPE), labelled `c2pa`, with one
/// child box per entry of `manifests` holding its bytes. The manifests are not
/// interpreted; equal entries give equal children, so a store whose manifests
/// begin with another's extends it.
pub fn c2pa_store(manifests: &[&[u8]]) -> Vec<u8> {
    superbox(crate::jumbf::C2PA_STORE_TYPE, Some("c2pa"), manifests)
}

/// The smallest [`c2pa_store`]: no manifests.
pub const C2PA_STORE_MIN_LEN: usize = 38;

/// A [`c2pa_store`] exactly `len` bytes long, for tests that need a size: one
/// manifest of filler (nonzero, varying) bytes, or none at
/// [`C2PA_STORE_MIN_LEN`].
///
/// Panics unless `len` is [`C2PA_STORE_MIN_LEN`] or at least 8 more (a child
/// box's header).
pub fn c2pa_store_of_len(len: usize) -> Vec<u8> {
    if len == C2PA_STORE_MIN_LEN {
        return c2pa_store(&[]);
    }
    let filler = len
        .checked_sub(C2PA_STORE_MIN_LEN + 8)
        .unwrap_or_else(|| panic!("a C2PA store of {len} bytes is too short to build"));
    let manifest: Vec<u8> = (0..filler).map(|i| (i * 7 % 251) as u8 + 1).collect();
    let store = c2pa_store(&[&manifest]);
    debug_assert_eq!(store.len(), len);
    store
}

/// JUMBF that is not a C2PA store, for tests: a superbox of type `jumbf_type`,
/// with no label, holding one child box per entry of `children`.
pub fn jumbf(jumbf_type: [u8; 16], children: &[&[u8]]) -> Vec<u8> {
    superbox(jumbf_type, None, children)
}

/// The XMP the checks write unless told otherwise: an xpacket, which every format
/// that holds XMP accepts.
const DEFAULT_XMP: &[u8] = b"<?xpacket begin=\"\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\
<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><!-- conformance --></x:xmpmeta>\
<?xpacket end=\"w\"?>";

/// A set of checks for one handler. Build it with [`new`](Self::new), say what the
/// format can hold, and run it on samples with [`check`](Self::check).
#[derive(Clone)]
pub struct Conformance {
    handler: Arc<dyn ContainerHandler>,
    xmp: Option<Vec<u8>>,
    c2pa: Option<Vec<u8>>,
    extra: Vec<(String, Arc<Updates>)>,
    mutations: bool,
}

impl Conformance {
    /// Checks for `handler`. By default it is expected to store XMP, and not to
    /// take a C2PA store, which has to be in a form the format accepts: set it with
    /// [`c2pa`](Self::c2pa).
    pub fn new(handler: Arc<dyn ContainerHandler>) -> Self {
        Self {
            handler,
            xmp: Some(DEFAULT_XMP.to_vec()),
            c2pa: None,
            extra: Vec::new(),
            mutations: true,
        }
    }

    /// The XMP packet to write and read back; `None` if the format cannot hold XMP.
    pub fn xmp(mut self, packet: Option<Vec<u8>>) -> Self {
        self.xmp = packet;
        self
    }

    /// The C2PA store to write and read back; `None` if the format does not take
    /// one.
    pub fn c2pa(mut self, store: Option<Vec<u8>>) -> Self {
        self.c2pa = store;
        self
    }

    /// Another update to run every check with, such as an option only this format
    /// has. A handler may refuse it for a sample (an error other than
    /// [`Error::UnsupportedFormat`]); what it writes is checked as for any other.
    pub fn updates(mut self, name: &str, updates: Updates) -> Self {
        self.extra.push((name.to_owned(), Arc::new(updates)));
        self
    }

    /// Whether to run the sample's truncations and byte flips (on by default).
    pub fn mutations(mut self, on: bool) -> Self {
        self.mutations = on;
        self
    }

    /// Run every check on `sample`, panicking on the first failure.
    pub fn check(&self, name: &str, sample: &[u8]) {
        if let Err(why) = self.try_check(name, sample) {
            panic!("{why}");
        }
    }

    /// Like [`check`](Self::check), but a sample the handler cannot parse passes:
    /// there is nothing to hold it to. For feeding arbitrary bytes, as a fuzzer does.
    pub fn check_if_parses(&self, name: &str, sample: &[u8]) {
        if self.handler.clone().parse(&mut Cursor::new(sample)).is_ok() {
            self.check(name, sample);
        }
    }

    /// Run every check on `sample`; the error says which failed and why.
    pub fn try_check(&self, name: &str, sample: &[u8]) -> std::result::Result<(), String> {
        self.run(sample).map_err(|why| format!("{name}: {why}"))
    }

    fn run(&self, sample: &[u8]) -> std::result::Result<(), String> {
        let s = self
            .handler
            .clone()
            .parse(&mut Cursor::new(sample))
            .map_err(|e| format!("does not parse: {e}"))?;
        tiles(&s, sample.len() as u64).map_err(|e| format!("parse: {e}"))?;
        parse_by_hand(&self.handler, sample, &s)?;

        // A handler that cannot write has only the parse to check.
        match s.write_plan(&mut Cursor::new(sample), &Updates::new()) {
            Err(Error::UnsupportedFormat) => return Ok(()),
            Err(e) => return Err(format!("cannot plan a write that changes nothing: {e}")),
            Ok(_) => {}
        }

        let keep = self
            .write(&s, sample, &Updates::new())?
            .ok_or("a write that changes nothing was refused")?;
        if keep.bytes != sample {
            return Err("a write that changes nothing changed the bytes".into());
        }
        if describe(&keep.structure) != describe(&s) {
            return Err("a write that changes nothing returned another structure".into());
        }

        for (name, updates, expect) in self.cases() {
            let Some(done) = self
                .write(&s, sample, &updates)
                .map_err(|e| format!("{name}: {e}"))?
            else {
                continue;
            };
            let src = &mut Cursor::new(&done.bytes);
            let wrote = self
                .handler
                .clone()
                .parse(src)
                .map_err(|e| format!("{name}: the output does not parse: {e}"))?;
            tiles(&wrote, done.bytes.len() as u64).map_err(|e| format!("{name}: output: {e}"))?;
            if describe(&wrote) != describe(&done.structure) {
                return Err(format!(
                    "{name}: the output is not the structure that was planned\n planned: {}\n parsed:  {}",
                    describe(&done.structure),
                    describe(&wrote)
                ));
            }
            let mut read = Collect::default();
            wrote
                .read(&mut Cursor::new(&done.bytes), &mut read)
                .map_err(|e| format!("{name}: reading the output: {e}"))?;
            read.check(&wrote, &done.bytes)
                .map_err(|e| format!("{name}: reading the output: {e}"))?;
            expect
                .check(&wrote, &done.bytes, &s, sample)
                .map_err(|e| format!("{name}: {e}"))?;

            // What was written, written again unchanged, is the same file.
            let again = self
                .write(&wrote, &done.bytes, &Updates::new())
                .map_err(|e| format!("{name}: rewriting the output: {e}"))?;
            if let Some(again) = again {
                if again.bytes != done.bytes {
                    return Err(format!("{name}: rewriting the output changed it"));
                }
            }
        }

        if let Some(store) = &self.c2pa {
            self.patching(&s, sample, store)
                .map_err(|e| format!("patching: {e}"))?;
        }
        self.composing(&s, sample)
            .map_err(|e| format!("compose: {e}"))?;

        if self.mutations {
            self.mutated(sample)?;
        }
        Ok(())
    }

    /// Sign the way a caller does: write `store` as a placeholder, then fill it in
    /// place with a signed store of the same length. Only bytes that a C2PA data
    /// hash excludes may change (the store's segments, where there is no data
    /// hash), and the output reads back with the signed store. A length that does
    /// not fit is refused, and changes nothing; `patch` zero-fills a shorter store.
    fn patching(
        &self,
        s: &Structure,
        sample: &[u8],
        store: &[u8],
    ) -> std::result::Result<(), String> {
        let updates = Updates::new().set_c2pa(store.to_vec());
        let Some(done) = self.write(s, sample, &updates)? else {
            return Ok(());
        };
        let item = done
            .structure
            .c2pa_item()
            .map_err(|e| e.to_string())?
            .ok_or("the written store is not an item")?;
        // Keep the superbox header and type, as a signed store would.
        let signed: Vec<u8> = store
            .iter()
            .enumerate()
            .map(|(i, b)| if i < 32 { *b } else { b ^ 0x5a })
            .collect();
        let mut out = Cursor::new(done.bytes.clone());
        match done.structure.patch_exact(&mut out, item, &signed) {
            Ok(()) => {}
            Err(Error::UnsupportedFeature(_)) => return Ok(()),
            Err(e) => return Err(format!("filling the placeholder failed: {e}")),
        }
        let patched = out.into_inner();
        if patched.len() != done.bytes.len() {
            return Err("filling the placeholder changed the length".into());
        }
        let may_change = match done.structure.c2pa_data_hash_exclusions(item) {
            Ok(ranges) => ranges,
            Err(_) => item
                .extents()
                .map(|x| done.structure.segment(x.segment()).map(|g| g.range()))
                .collect::<Result<Vec<_>>>()
                .map_err(|e| e.to_string())?,
        };
        let changed = (0..patched.len()).find(|&i| {
            patched[i] != done.bytes[i]
                && !may_change
                    .iter()
                    .any(|r| (r.offset()..r.end()).contains(&(i as u64)))
        });
        if let Some(i) = changed {
            return Err(format!(
                "filling the placeholder changed byte {i}, which is hashed"
            ));
        }
        let reread = self
            .handler
            .clone()
            .parse(&mut Cursor::new(&patched))
            .map_err(|e| format!("the filled output does not parse: {e}"))?;
        if reread
            .c2pa_store(&mut Cursor::new(&patched))
            .map_err(|e| e.to_string())?
            != Some(signed.clone())
        {
            return Err("the filled output does not read back as the signed store".into());
        }

        // `patch` zero-fills a shorter store, if the format allows one, and refuses a
        // longer one; either way a refusal changes nothing.
        if *item.transfer() == crate::TransferEncoding::Identity {
            let short = &signed[..signed.len() / 2];
            let mut out = Cursor::new(done.bytes.clone());
            match done.structure.patch(&mut out, item, short) {
                Ok(()) => {
                    let out = out.into_inner();
                    let mut padded = short.to_vec();
                    padded.resize(signed.len(), 0);
                    let reread = self
                        .handler
                        .clone()
                        .parse(&mut Cursor::new(&out))
                        .map_err(|e| format!("the patched output does not parse: {e}"))?;
                    if reread
                        .c2pa_store(&mut Cursor::new(&out))
                        .map_err(|e| e.to_string())?
                        != Some(padded)
                    {
                        return Err("a shorter patch is not the store and zeros".into());
                    }
                }
                Err(_) if out.into_inner() == done.bytes => {}
                Err(_) => return Err("a refused patch changed the output".into()),
            }
            let longer = [&signed[..], &[1]].concat();
            let mut out = Cursor::new(done.bytes.clone());
            if done.structure.patch(&mut out, item, &longer).is_ok() {
                return Err("a patch longer than the placeholder was accepted".into());
            }
            if out.into_inner() != done.bytes {
                return Err("a refused patch changed the output".into());
            }
        }

        // Three bytes more or fewer change the stored length, base64 included.
        let longer = [&signed[..], &[0; 3]].concat();
        for wrong in [&signed[..signed.len().saturating_sub(3)], &longer[..]] {
            let mut out = Cursor::new(done.bytes.clone());
            if done.structure.patch_exact(&mut out, item, wrong).is_ok() {
                return Err(format!(
                    "a store of {} bytes filled a placeholder of {}",
                    wrong.len(),
                    signed.len()
                ));
            }
            if out.into_inner() != done.bytes {
                return Err("a refused fill changed the output".into());
            }
        }
        Ok(())
    }

    /// What [`ContainerHandler::compose`] gives for an item is exactly what a
    /// write inserts: bytes that start where the item's first segment does and
    /// cover all its segments, framing included (and anything the format adds
    /// after the item, such as GIF's XMP trailer).
    fn composing(&self, s: &Structure, sample: &[u8]) -> std::result::Result<(), String> {
        for (kind, data) in [(ItemKind::Xmp, &self.xmp), (ItemKind::C2pa, &self.c2pa)] {
            let Some(data) = data else { continue };
            let Ok(composed) = self.handler.compose(kind, data) else {
                continue;
            };
            let updates = match kind {
                ItemKind::Xmp => Updates::new().set_xmp(data.clone()),
                _ => Updates::new().set_c2pa(data.clone()),
            };
            let Some(done) = self.write(s, sample, &updates)? else {
                continue;
            };
            // The item written, not one the sample keeps elsewhere (HEIF XMP in mdat).
            let item = done
                .structure
                .items_of(kind)
                .find(|e| {
                    e.extents().all(|x| {
                        done.structure
                            .segment(x.segment())
                            .is_ok_and(|g| g.kind() == SegmentKind::Metadata)
                    })
                })
                .ok_or_else(|| format!("no {kind} item was written"))?;
            let range = |x: &crate::item::Extent| {
                done.structure
                    .segment(x.segment())
                    .map(|g| g.range())
                    .map_err(|e| e.to_string())
            };
            let first = item.extents().next().ok_or("an item without extents")?;
            let start = range(first)?.offset() as usize;
            let mut end = start;
            for x in item.extents() {
                end = end.max(range(x)?.end() as usize);
            }
            let written = done.bytes.get(start..start + composed.len());
            if start + composed.len() < end || written != Some(&composed[..]) {
                return Err(format!("{kind} is not what a write inserts"));
            }
        }
        Ok(())
    }

    /// The updates every sample is written with, and what each must do.
    fn cases(&self) -> Vec<(String, Arc<Updates>, Expect)> {
        let mut cases = vec![];
        let mut add =
            |name: &str, u: Updates, e: Expect| cases.push((name.to_owned(), Arc::new(u), e));
        if let Some(x) = &self.xmp {
            add(
                "set xmp",
                Updates::new().set_xmp(x.clone()),
                Expect {
                    xmp: Want::Is(x.clone()),
                    c2pa: Want::Same,
                },
            );
            add(
                "remove xmp",
                Updates::new().remove_xmp(),
                Expect {
                    xmp: Want::Absent,
                    c2pa: Want::Same,
                },
            );
        }
        if let Some(j) = &self.c2pa {
            add(
                "set c2pa",
                Updates::new().set_c2pa(j.clone()),
                Expect {
                    xmp: Want::Same,
                    c2pa: Want::Is(j.clone()),
                },
            );
            add(
                "remove c2pa",
                Updates::new().remove_c2pa(),
                Expect {
                    xmp: Want::Same,
                    c2pa: Want::Absent,
                },
            );
        }
        if let (Some(x), Some(j)) = (&self.xmp, &self.c2pa) {
            add(
                "set both",
                Updates::new().set_xmp(x.clone()).set_c2pa(j.clone()),
                Expect {
                    xmp: Want::Is(x.clone()),
                    c2pa: Want::Is(j.clone()),
                },
            );
        }
        add(
            "remove all",
            Updates::remove_all(),
            Expect {
                xmp: Want::Absent,
                c2pa: Want::Absent,
            },
        );
        for (name, u) in &self.extra {
            cases.push((
                name.clone(),
                u.clone(),
                Expect {
                    xmp: Want::Any,
                    c2pa: Want::Any,
                },
            ));
        }
        cases
    }

    /// Write with `updates` every way there is, checking they agree. `None` if the
    /// handler refuses the update for this sample.
    fn write(
        &self,
        s: &Structure,
        sample: &[u8],
        updates: &Updates,
    ) -> std::result::Result<Option<Written>, String> {
        match write_every_way(s, sample, updates)? {
            Ok(written) => Ok(Some(written)),
            Err(Error::UnsupportedFormat) => {
                Err("a write the format should take is unsupported".into())
            }
            Err(_) => Ok(None),
        }
    }

    /// Damaged input must be an error or a structure that is still sound; never
    /// a panic.
    fn mutated(&self, sample: &[u8]) -> std::result::Result<(), String> {
        let mut inputs: Vec<(String, Vec<u8>)> = Vec::new();
        let n = sample.len();
        let mut cuts: Vec<usize> = (0..n.min(48)).collect();
        cuts.extend((1..=48).map(|i| n * i / 49));
        cuts.sort_unstable();
        cuts.dedup();
        for cut in cuts {
            inputs.push((format!("truncated to {cut} bytes"), sample[..cut].to_vec()));
        }
        let mut rng = 0x9e37_79b9_7f4a_7c15u64;
        for _ in 0..96.min(n * 2) {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let at = (rng % n.max(1) as u64) as usize;
            let mut v = sample.to_vec();
            if let Some(b) = v.get_mut(at) {
                *b ^= (rng >> 32) as u8 | 1;
            }
            inputs.push((format!("byte {at} changed"), v));
        }
        for (what, bytes) in inputs {
            let tried = catch_unwind(AssertUnwindSafe(|| self.damaged(&bytes)));
            match tried {
                Err(_) => return Err(format!("panicked on input {what}")),
                Ok(Err(why)) => return Err(format!("on input {what}: {why}")),
                Ok(Ok(())) => {}
            }
        }
        Ok(())
    }

    fn damaged(&self, bytes: &[u8]) -> std::result::Result<(), String> {
        let Ok(s) = self.handler.clone().parse(&mut Cursor::new(bytes)) else {
            return Ok(());
        };
        tiles(&s, bytes.len() as u64).map_err(|e| format!("parse: {e}"))?;
        let mut updates = vec![Updates::new()];
        if let Some(x) = &self.xmp {
            updates.push(Updates::new().set_xmp(x.clone()));
        }
        if let Some(j) = &self.c2pa {
            updates.push(Updates::new().set_c2pa(j.clone()));
        }
        updates.push(Updates::remove_all());
        for u in &updates {
            if let Ok(plan) = s.write_plan(&mut Cursor::new(bytes), u) {
                validate(&plan, bytes.len() as u64)
                    .map_err(|e| format!("the plan does not tile: {e}"))?;
                // Reading damaged data may fail; it must not misbehave.
                let _ = plan.execute(
                    &mut Cursor::new(bytes),
                    &mut Vec::new(),
                    &mut crate::NullSink,
                );
            }
        }
        Ok(())
    }
}

struct Written {
    bytes: Vec<u8>,
    structure: Structure,
}

enum Want {
    /// Whatever the source had.
    Same,
    /// This value.
    Is(Vec<u8>),
    /// Nothing.
    Absent,
    /// Not checked.
    Any,
}

struct Expect {
    xmp: Want,
    c2pa: Want,
}

impl Expect {
    fn check(
        &self,
        out: &Structure,
        bytes: &[u8],
        src: &Structure,
        source: &[u8],
    ) -> std::result::Result<(), String> {
        type Item = std::result::Result<Option<Vec<u8>>, String>;
        let read = |s: &Structure, b: &[u8]| -> (Item, Item) {
            (
                s.xmp(&mut Cursor::new(b))
                    .map_err(|e| format!("reading XMP: {e}")),
                s.c2pa_store(&mut Cursor::new(b))
                    .map_err(|e| format!("reading the C2PA store: {e}")),
            )
        };
        let (xmp, c2pa) = read(out, bytes);
        let (xmp0, c2pa0) = read(src, source);
        for (what, kind, want, got, before) in [
            ("XMP", ItemKind::Xmp, &self.xmp, xmp, xmp0),
            ("C2PA store", ItemKind::C2pa, &self.c2pa, c2pa, c2pa0),
        ] {
            match want {
                Want::Any => {}
                // A source whose item cannot be read (a corrupt or ambiguous one) has
                // nothing to compare with.
                Want::Same => {
                    if let Ok(before) = before {
                        if got? != before {
                            return Err(format!("{what} changed but was not asked to"));
                        }
                    }
                }
                Want::Is(v) => {
                    let got = got?;
                    if got.as_deref() != Some(v.as_slice()) {
                        return Err(format!(
                            "{what} is not what was set: read back {:?}",
                            got.map(|g| g.len())
                        ));
                    }
                }
                Want::Absent => {
                    if let Some(got) = got? {
                        if !carried(out, bytes, kind, &got) {
                            return Err(format!("{what} is still there after it was removed"));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// Whether `data` is an item of `kind` outside the metadata segments, which
/// [`Updates`] does not edit: an XMP item that a HEIF file keeps in `mdat` is carried
/// through a write unchanged.
fn carried(s: &Structure, bytes: &[u8], kind: ItemKind, data: &[u8]) -> bool {
    s.items_of(kind).any(|e| {
        e.extents().all(|x| {
            s.segment(x.segment())
                .is_ok_and(|g| g.kind() != SegmentKind::Metadata)
        }) && s
            .read_item(&mut Cursor::new(bytes), e)
            .is_ok_and(|d| d == data)
    })
}

/// Everything a structure says, as text, for comparing two of them.
fn describe(s: &Structure) -> String {
    let mut out = String::new();
    for seg in s.segments() {
        out.push_str(&format!("{seg:?}\n"));
    }
    for sub in s.subassets() {
        out.push_str(&format!(
            "sub {:?} {:?} {:?}\n",
            sub.range(),
            sub.segments(),
            sub.media_type()
        ));
    }
    out.push_str(&format!(
        "trailing {:?} total {} {}\n",
        s.trailing(),
        s.total_size(),
        s.media_type()
    ));
    // Items in file order: how a handler declares them is not part of what a
    // structure says.
    let mut items: Vec<_> = s.items().collect();
    items.sort_by_key(|e| e.extents().next().map(|p| p.range().offset()));
    for e in items {
        out.push_str(&format!("item {:?}", e.info()));
        for p in e.extents() {
            out.push_str(&format!(" {:?}:{:?}", p.segment(), p.range()));
        }
        out.push('\n');
    }
    out
}

/// The segments follow one another from the first byte to the last, and an
/// item's extents lie in their segments.
fn tiles(s: &Structure, len: u64) -> std::result::Result<(), String> {
    let mut at = 0u64;
    for (i, seg) in s.segments().iter().enumerate() {
        let e = seg.range();
        if e.offset() != at {
            return Err(format!("segment {i} starts at {}, not {at}", e.offset()));
        }
        at = e.end();
    }
    if at != len || s.total_size() != len {
        return Err(format!(
            "segments end at {at} and the structure says {} for a stream of {len}",
            s.total_size()
        ));
    }
    // The finer elements (JPEG restart intervals, say) tile the stream too.
    let mut at = 0u64;
    s.elements(|_, e| {
        let r = e.range();
        if r.offset() != at {
            return Err(Error::InvalidFormat(format!(
                "an element starts at {}, not {at}",
                r.offset()
            )));
        }
        at = r.end();
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    if at != len {
        return Err(format!("elements end at {at}, not {len}"));
    }
    for e in s.items() {
        for p in e.extents() {
            let seg = s.segment(p.segment()).map_err(|e| e.to_string())?;
            let (r, x) = (p.range(), seg.range());
            if r.offset() < x.offset() || r.end() > x.end() {
                return Err(format!(
                    "{} item has an extent outside its segment",
                    e.kind()
                ));
            }
        }
    }
    Ok(())
}

/// Parse with the reads answered by hand, each exactly as large as asked, which
/// shows a handler that depends on how much a read returns.
fn parse_by_hand(
    handler: &Arc<dyn ContainerHandler>,
    sample: &[u8],
    want: &Structure,
) -> std::result::Result<(), String> {
    let h = handler.clone();
    let mut task = Task::new(sample.len() as u64, |io| h.parse_async(io));
    let got = loop {
        match task
            .resume()
            .map_err(|e| format!("parse with reads answered by hand: {e}"))?
        {
            Progress::Read(mut req) => {
                let from = (req.offset() as usize).min(sample.len());
                let to = (from + req.min_len()).min(sample.len());
                req.buffer().extend_from_slice(&sample[from..to]);
            }
            Progress::Done(s) => break s,
        }
    };
    if describe(&got) != describe(want) {
        return Err("parsing with reads answered by hand gives another structure".into());
    }
    Ok(())
}

/// Carry out a plan through the stepper, answering its reads from `source`.
fn step_by_step(plan: &WritePlan<'_>, source: &[u8]) -> std::result::Result<Vec<u8>, String> {
    let mut run = plan.start();
    let mut out = Vec::new();
    loop {
        match run
            .step(&mut crate::NullSink)
            .map_err(|e| format!("stepper: {e}"))?
        {
            Action::Read { offset, buf } => {
                let at = offset as usize;
                let src = source
                    .get(at..at + buf.len())
                    .ok_or("the plan reads past the source")?;
                buf.copy_from_slice(src);
            }
            Action::Write(b) => out.extend_from_slice(b),
            Action::Done => return Ok(out),
        }
    }
}

/// A sink that wants only headers and trailers.
#[derive(Default)]
struct Pick(Vec<u8>);

impl Sink for Pick {
    fn wants(&self, _: &crate::segment::Segment, role: Role) -> bool {
        role != Role::Payload
    }

    fn chunk(&mut self, c: Chunk<'_>) -> Result<()> {
        self.0.extend_from_slice(c.data);
        Ok(())
    }
}

/// A sink that skips runs still gets the rest, which are the bytes a walk over the
/// finished structure finds: two independent routes to the same hash.
fn selective_sink(
    s: &Structure,
    sample: &[u8],
    updates: &Updates,
    out: &Structure,
    bytes: &[u8],
) -> std::result::Result<(), String> {
    let mut pick = Pick::default();
    s.write(
        &mut Cursor::new(sample),
        &mut Vec::new(),
        updates,
        &mut pick,
    )
    .map_err(|e| format!("write with a selective sink: {e}"))?;
    let mut walked = Vec::new();
    for r in out.regions().filter(|r| r.role != Role::Payload) {
        let (from, to) = (r.range.offset() as usize, r.range.end() as usize);
        walked.extend_from_slice(
            bytes
                .get(from..to)
                .ok_or("a region lies outside the output")?,
        );
    }
    if pick.0 != walked {
        return Err(
            "a sink that skips payloads did not get the headers and trailers a walk finds".into(),
        );
    }
    Ok(())
}

/// A sink that keeps what it is given.
#[derive(Default)]
struct Collect {
    chunks: Vec<(usize, Role, u64, usize)>,
    data: Vec<u8>,
}

impl Sink for Collect {
    fn chunk(&mut self, c: Chunk<'_>) -> Result<()> {
        self.chunks
            .push((c.segment.index(), c.role, c.offset, c.data.len()));
        self.data.extend_from_slice(c.data);
        Ok(())
    }
}

impl Collect {
    /// Every output byte arrived once, in order, in the segment and role the
    /// structure gives it.
    fn check(&self, out: &Structure, bytes: &[u8]) -> std::result::Result<(), String> {
        if self.data != bytes {
            return Err("the sink did not see the bytes that were written".into());
        }
        let mut at = 0u64;
        for &(seg, role, offset, len) in &self.chunks {
            if offset != at {
                return Err(format!("the sink got a chunk at {offset}, expecting {at}"));
            }
            let s = out
                .segments()
                .get(seg)
                .ok_or("the sink got a chunk of a segment that does not exist")?;
            let region = match role {
                Role::Header => s.header(),
                Role::Payload => s.payload(),
                _ => s.trailer(),
            };
            if offset < region.offset() || offset + len as u64 > region.end() {
                return Err(format!("the sink got bytes at {offset} for {role:?} of segment {seg}, which is elsewhere"));
            }
            at += len as u64;
        }
        if at != out.total_size() {
            return Err("the sink did not see the whole output".into());
        }
        Ok(())
    }
}

/// Write `sample`, parsed as `s`, with `updates` every way there is: the plan,
/// `write` with a sink that keeps everything and one that skips, `Structure::plan`
/// and the stepper. The outer error says how they disagree; the inner one is the
/// handler refusing the update, as every way agreed.
fn write_every_way(
    s: &Structure,
    sample: &[u8],
    updates: &Updates,
) -> std::result::Result<Result<Written>, String> {
    let plan = match s.write_plan(&mut Cursor::new(sample), updates) {
        Ok(p) => p,
        Err(refused) => {
            // A refused update is refused whichever way it is asked for.
            return match s.write(
                &mut Cursor::new(sample),
                &mut Vec::new(),
                updates,
                &mut crate::NullSink,
            ) {
                Err(e @ Error::UnsupportedFormat) => Ok(Err(e)),
                Err(_) => Ok(Err(refused)),
                Ok(_) => Err("write succeeded where write_plan failed".into()),
            };
        }
    };
    validate(&plan, sample.len() as u64)
        .map_err(|e| format!("the plan does not tile its structure: {e}"))?;

    let mut bytes = Vec::new();
    let mut sink = Collect::default();
    let structure = match s.write(&mut Cursor::new(sample), &mut bytes, updates, &mut sink) {
        Ok(structure) => structure,
        // Some refusals are found only while the plan runs, such as an offset
        // that no longer fits its field; running it step by step must find it too.
        Err(e) => {
            return match step_by_step(&plan, sample) {
                Err(_) => Ok(Err(e)),
                Ok(_) => Err(format!(
                    "write failed after a plan, but the plan runs step by step: {e}"
                )),
            }
        }
    };
    if describe(&structure) != describe(plan.structure()) {
        return Err("write returned a structure other than the plan's".into());
    }
    if bytes.len() as u64 != structure.total_size() {
        return Err(format!(
            "wrote {} bytes for a structure of {}",
            bytes.len(),
            structure.total_size()
        ));
    }
    let planned = s
        .plan(&mut Cursor::new(sample), updates)
        .map_err(|e| format!("plan failed after a plan: {e}"))?;
    if describe(&planned) != describe(&structure) {
        return Err("Structure::plan is not what write returns".into());
    }
    sink.check(&structure, &bytes)?;
    selective_sink(s, sample, updates, &structure, &bytes)?;

    if step_by_step(&plan, sample)? != bytes {
        return Err("running the plan step by step wrote other bytes than write".into());
    }
    Ok(Ok(Written { bytes, structure }))
}

/// Parse `source` with `handler`, write it with `updates`, and return the output
/// with the structure the write returned, for container tests. The write is made
/// every way there is (plan, write, stepper, sinks); panics if they disagree, or if
/// the output does not parse to the structure returned. A refused update is the
/// error.
pub fn rewrite(
    handler: Arc<dyn ContainerHandler>,
    source: &[u8],
    updates: &Updates,
) -> Result<(Vec<u8>, Structure)> {
    let structure = handler.clone().parse(&mut Cursor::new(source))?;
    let written = write_every_way(&structure, source, updates)
        .unwrap_or_else(|e| panic!("the ways to write disagree: {e}"))?;
    let again = handler
        .parse(&mut Cursor::new(&written.bytes))
        .unwrap_or_else(|e| panic!("the output does not parse: {e}"));
    tiles(&again, written.bytes.len() as u64)
        .unwrap_or_else(|e| panic!("the output does not tile: {e}"));
    assert_eq!(
        describe(&written.structure),
        describe(&again),
        "the structure a write returns is not the output's"
    );
    Ok((written.bytes, written.structure))
}
