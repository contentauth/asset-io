use std::{env, fs::File};

use asset_io::{HandlerRegistry, Structure};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let path = args.next().ok_or("usage: inspect FILE [FORMAT]")?;
    let format = args.next();
    let structure = Structure::parse(
        &HandlerRegistry::builtin(),
        &mut File::open(path)?,
        format.as_deref(),
    )?;
    println!("{}", asset_io_wasm::inspection_summary(&structure));
    Ok(())
}
