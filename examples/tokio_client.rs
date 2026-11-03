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

//! Use asset-io from tokio, with async file I/O.
//!
//! The `asset_io::tokio` adapters handle parsing, item reads, planning and writing
//! with async stream I/O. The sans-I/O requests and write steps stay inside the
//! adapters. Each file's work can be spawned on the multi-threaded runtime.
//!
//! Run:
//!
//! ```text
//! cargo run --example tokio_client --features all-formats,tokio -- inspect a.jpg b.png c.mp4
//! cargo run --example tokio_client --features all-formats,tokio -- edit in.jpg out.jpg new.xmp
//! ```

use std::time::Instant;

use asset_io::{tokio as asset, Chunk, HandlerRegistry, ItemKind, Result, Structure, Updates};
use sha2::{Digest, Sha256};
use tokio::fs::File;

/// Parse a file and describe it.
async fn inspect(path: String) -> Result<String> {
    let registry = HandlerRegistry::builtin();
    let mut file = File::open(&path).await?;
    let structure = asset::parse(&registry, &mut file, None).await?;
    let mut out = format!(
        "{path}\n  {} ({}), {} bytes, {} segments\n",
        structure.media_type(),
        structure.container_id(),
        structure.total_size(),
        structure.segments().len()
    );
    for e in structure.items() {
        let bytes = asset::read_item(&structure, &mut file, e).await?;
        let label = e.label().map(|l| format!(" [{l}]")).unwrap_or_default();
        out += &format!("  {}{label}: {} bytes\n", e.kind(), bytes.len());
    }
    Ok(out)
}

/// Replace the XMP of `input`, writing `output`, and hash the output as it is
/// written. Returns the SHA-256 of the output.
async fn edit(input: String, output: String, xmp: Vec<u8>) -> Result<(Structure, String)> {
    let started = Instant::now();
    let registry = HandlerRegistry::builtin();
    let mut src = File::open(&input).await?;
    let parse_started = Instant::now();
    let structure = asset::parse(&registry, &mut src, None).await?;
    let parse_elapsed = parse_started.elapsed();

    // Plan the write: the structure of the output and the steps that produce it.
    let plan_started = Instant::now();
    let updates = Updates::new().set_xmp(xmp);
    let plan = asset::write_plan(&structure, &mut src, &updates).await?;
    let plan_elapsed = plan_started.elapsed();

    // Run the plan. Every output byte also goes to a sink, tagged with its segment
    // and role, so it can be hashed without a second pass.
    let write_started = Instant::now();
    let mut out = File::create(&output).await?;
    let mut hash = Sha256::new();
    let mut sink = |chunk: Chunk<'_>| {
        hash.update(chunk.data);
        Ok(())
    };
    asset::execute(&plan, &mut src, &mut out, &mut sink).await?;
    let digest: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    let write_elapsed = write_started.elapsed();

    // The layout of what was written is known without parsing it again.
    let written = plan.into_structure();
    let xmp_items = written.items_of(ItemKind::Xmp).count();
    let total_elapsed = started.elapsed();
    println!(
        "  wrote {} bytes, {xmp_items} XMP packet(s)",
        written.total_size()
    );
    println!(
        "  timing: parse {:.3} ms, plan {:.3} ms, write+sha256 {:.3} ms, total {:.3} ms",
        parse_elapsed.as_secs_f64() * 1000.0,
        plan_elapsed.as_secs_f64() * 1000.0,
        write_elapsed.as_secs_f64() * 1000.0,
        total_elapsed.as_secs_f64() * 1000.0,
    );
    Ok((written, digest))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("inspect") if args.len() > 1 => {
            // One task per file, all running at once. Each can move between threads.
            let handles: Vec<_> = args[1..]
                .iter()
                .cloned()
                .map(|path| tokio::spawn(inspect(path)))
                .collect();
            for h in handles {
                match h.await.expect("task panicked") {
                    Ok(text) => print!("{text}"),
                    Err(e) => eprintln!("error: {e}"),
                }
            }
        }
        Some("edit") if args.len() == 4 => {
            let xmp = tokio::fs::read(&args[3]).await?;
            let (_, digest) = tokio::spawn(edit(args[1].clone(), args[2].clone(), xmp))
                .await
                .expect("task panicked")?;
            println!("  sha256 {digest}");
        }
        _ => {
            eprintln!("Usage:");
            eprintln!("  tokio_client inspect <file>...");
            eprintln!("  tokio_client edit <in> <out> <xmp-file>");
            std::process::exit(2);
        }
    }
    Ok(())
}
