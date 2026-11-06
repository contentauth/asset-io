use std::{
    collections::BTreeSet,
    error::Error,
    ffi::OsStr,
    io::{self, Write},
    path::{Path, PathBuf},
};

use asset_io::{
    tokio as asset, C2paHash, HandlerRegistry, Item, ItemKind, ScanLimits, Segment, Structure,
    TransferEncoding, Updates,
};
use clap::{Args, Parser, Subcommand};
use serde_json::{json, Value};
use tokio::{fs, io::AsyncWriteExt};

type CliResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Debug, Parser)]
#[command(
    name = "aio",
    version,
    about = "Inspect and update media assets using asset-io"
)]
struct Cli {
    /// Print a human-readable report (the default for inspection commands).
    #[arg(long, global = true, conflicts_with = "json")]
    human: bool,

    /// Print a versioned JSON array instead of a readable report.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

impl Command {
    fn name(&self) -> &'static str {
        match self {
            Self::Inspect { .. } => "inspect",
            Self::Items { .. } => "items",
            Self::Extract { .. } => "extract",
            Self::Update { .. } => "update",
            Self::Embedded { .. } => "embedded",
        }
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Describe asset structure and available items.
    Inspect {
        #[arg(required = true, value_name = "PATH|GLOB")]
        inputs: Vec<String>,
        /// Show physical segments instead of the element hierarchy.
        #[arg(long)]
        segments: bool,
        /// Limit the displayed element hierarchy depth.
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..), conflicts_with = "segments")]
        max_depth: Option<u32>,
    },
    /// List logical items without reading their content.
    Items {
        #[arg(required = true, value_name = "PATH|GLOB")]
        inputs: Vec<String>,
        #[command(flatten)]
        selection: Selection,
    },
    /// Extract item bytes to files. Decoded content is the default.
    Extract {
        #[arg(required = true, value_name = "PATH|GLOB")]
        inputs: Vec<String>,
        /// Directory to receive one subdirectory per matched asset.
        #[arg(short, long, value_name = "DIR", required_unless_present = "output")]
        output_dir: Option<PathBuf>,
        /// Destination for exactly one selected item from one matched asset.
        #[arg(long, value_name = "FILE", conflicts_with = "output_dir")]
        output: Option<PathBuf>,
        #[command(flatten)]
        selection: Selection,
        /// Replace existing output files. Inputs are never overwritten.
        #[arg(long)]
        force: bool,
        /// Extract stored bytes rather than decoded item content.
        #[arg(long)]
        stored: bool,
    },
    /// Replace or remove XMP and/or JUMBF metadata.
    Update {
        #[arg(required = true, value_name = "PATH|GLOB")]
        inputs: Vec<String>,
        /// File containing replacement XMP bytes.
        #[arg(long, value_name = "FILE", conflicts_with = "remove_xmp")]
        xmp_file: Option<PathBuf>,
        /// Remove XMP.
        #[arg(long)]
        remove_xmp: bool,
        /// File containing replacement JUMBF bytes.
        #[arg(long, value_name = "FILE", conflicts_with = "remove_jumbf")]
        jumbf_file: Option<PathBuf>,
        /// Remove all JUMBF items.
        #[arg(long)]
        remove_jumbf: bool,
        /// Output file, for a single matched input.
        #[arg(short, long, value_name = "FILE", conflicts_with = "output_dir")]
        output: Option<PathBuf>,
        /// Output directory, required for multiple matched inputs.
        #[arg(long, value_name = "DIR")]
        output_dir: Option<PathBuf>,
        /// Replace existing output files. Inputs are never overwritten.
        #[arg(long)]
        force: bool,
    },
    /// Recursively discover and describe embedded assets.
    Embedded {
        #[arg(required = true, value_name = "PATH|GLOB")]
        inputs: Vec<String>,
        #[arg(long, default_value_t = 4)]
        max_depth: usize,
        #[arg(long, default_value_t = 64)]
        max_assets: usize,
        #[arg(long, default_value_t = 256 * 1024 * 1024)]
        max_total_bytes: u64,
    },
}

#[derive(Debug, Default, Args)]
struct Selection {
    /// Select a zero-based item index from the current file.
    #[arg(long)]
    item: Option<usize>,
    /// Filter by kind: xmp, jumbf, exif, embedded_asset, or other.
    #[arg(long, value_parser = parse_item_kind)]
    kind: Option<ItemKind>,
    /// Filter by an exact, case-sensitive label.
    #[arg(long)]
    label: Option<String>,
}

impl Selection {
    fn select<'a>(&self, structure: &'a Structure) -> CliResult<Vec<(usize, &'a Item)>> {
        let items: Vec<_> = structure
            .items()
            .enumerate()
            .filter(|(index, item)| {
                self.item.is_none_or(|selected| selected == *index)
                    && self.kind.is_none_or(|kind| kind == item.kind())
                    && self
                        .label
                        .as_deref()
                        .is_none_or(|label| item.label() == Some(label))
            })
            .collect();
        if self.item.is_some() && items.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "selected item does not exist or does not match the filters",
            )
            .into());
        }
        Ok(items)
    }
}

fn parse_item_kind(value: &str) -> Result<ItemKind, String> {
    match value {
        "xmp" => Ok(ItemKind::Xmp),
        "jumbf" => Ok(ItemKind::Jumbf),
        "exif" => Ok(ItemKind::Exif),
        "embedded_asset" => Ok(ItemKind::EmbeddedAsset),
        "other" => Ok(ItemKind::Other),
        _ => Err("expected xmp, jumbf, exif, embedded_asset, or other".into()),
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let code = match run(cli).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("aio: {error}");
            2
        }
    };
    std::process::ExitCode::from(code as u8)
}

async fn run(cli: Cli) -> CliResult<i32> {
    let (patterns, output_dir) = match &cli.command {
        Command::Inspect { inputs, .. }
        | Command::Items { inputs, .. }
        | Command::Embedded { inputs, .. } => (inputs, None),
        Command::Extract {
            inputs, output_dir, ..
        } => (inputs, output_dir.as_ref()),
        Command::Update {
            inputs, output_dir, ..
        } => (inputs, output_dir.as_ref()),
    };
    let paths = expand_inputs(patterns)?;
    match &cli.command {
        Command::Extract {
            output: Some(_), ..
        }
        | Command::Update {
            output: Some(_), ..
        } if paths.len() != 1 => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--output requires one matched input; use --output-dir for multiple inputs",
            )
            .into());
        }
        _ => {}
    }
    if let Some(dir) = output_dir {
        fs::create_dir_all(dir).await?;
    }

    let registry = HandlerRegistry::builtin();
    let mut results = Vec::with_capacity(paths.len());
    let mut failed = false;
    for (index, path) in paths.iter().enumerate() {
        let result = match &cli.command {
            Command::Inspect {
                max_depth,
                segments,
                ..
            } => inspect_one(path, &registry, !segments, *max_depth).await,
            Command::Items { selection, .. } => items_one(path, &registry, selection).await,
            Command::Extract {
                output_dir,
                output,
                selection,
                stored,
                force,
                ..
            } => {
                extract_one(
                    path,
                    index,
                    output_dir.as_deref(),
                    output.as_deref(),
                    selection,
                    *stored,
                    *force,
                    &paths,
                    &registry,
                )
                .await
            }
            Command::Update {
                xmp_file,
                remove_xmp,
                jumbf_file,
                remove_jumbf,
                output,
                output_dir,
                force,
                ..
            } => {
                update_one(
                    path,
                    index,
                    paths.len(),
                    output.as_deref(),
                    output_dir.as_deref(),
                    xmp_file.as_deref(),
                    *remove_xmp,
                    jumbf_file.as_deref(),
                    *remove_jumbf,
                    *force,
                    &paths,
                    &registry,
                )
                .await
            }
            Command::Embedded {
                max_depth,
                max_assets,
                max_total_bytes,
                ..
            } => {
                embedded_one(
                    path.clone(),
                    ScanLimits {
                        max_depth: *max_depth,
                        max_assets: *max_assets,
                        max_total_bytes: *max_total_bytes,
                    },
                )
                .await
            }
        };
        match result {
            Ok(value) => results.push(value),
            Err(error) => {
                failed = true;
                results.push(json!({
                    "schema_version": 1,
                    "status": "error",
                    "operation": cli.command.name(),
                    "path": path.display().to_string(),
                    "error": error.to_string(),
                }));
            }
        }
    }

    let inspection = matches!(
        cli.command,
        Command::Inspect { .. } | Command::Items { .. } | Command::Embedded { .. }
    );
    if cli.human || (inspection && !cli.json) {
        print_human(&cli.command, &results);
    } else {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        serde_json::to_writer_pretty(&mut out, &results)?;
        out.write_all(b"\n")?;
        out.flush()?;
    }
    Ok(if failed { 1 } else { 0 })
}

fn expand_inputs(patterns: &[String]) -> CliResult<Vec<PathBuf>> {
    let mut paths = BTreeSet::new();
    for pattern in patterns {
        let matches = glob::glob(pattern)?;
        let mut found = false;
        for path in matches {
            paths.insert(path?);
            found = true;
        }
        if !found {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("input pattern matched no paths: {pattern}"),
            )
            .into());
        }
    }
    Ok(paths.into_iter().collect())
}

fn format_hint(path: &Path) -> Option<String> {
    path.extension().and_then(OsStr::to_str).map(str::to_owned)
}

async fn inspect_one(
    path: &Path,
    registry: &HandlerRegistry,
    tree: bool,
    max_depth: Option<u32>,
) -> CliResult<Value> {
    let file = fs::File::open(path).await?;
    let asset = asset::Asset::open_with(registry, file, format_hint(path).as_deref()).await?;
    let (structure, mut source) = asset.into_parts();
    let items = describe_items(&structure, &mut source, &Selection::default()).await?;
    let mut result = structure_json(path, &structure, items);
    let mut source = source.into_std().await;
    let registry = registry.clone();
    tokio::task::spawn_blocking(move || {
        if tree {
            let mut nodes = Vec::new();
            structure.walk(&mut source, |node| {
                if max_depth.is_none_or(|depth| node.depth <= depth) {
                    nodes.push(json!({
                        "path": node.path,
                        "name": node.name.to_string(),
                        "box_name": structure.box_name(node.name).map(|name| name.into_owned()),
                        "depth": node.depth,
                        "range": range_json(node.range),
                        "header_bytes": node.header_len,
                        "version": node.version,
                        "flags": node.flags,
                        "user_type": node.user_type.map(|ty| hex(&ty)),
                    }));
                }
                Ok(())
            })?;
            result["nodes"] = json!(nodes);
        }
        let embedded = embedded_json(&structure, &mut source, &registry, ScanLimits::default())?;
        result["embedded_count"] = json!(embedded.len());
        result["embedded"] = json!(embedded);
        Ok::<_, Box<dyn Error + Send + Sync>>(result)
    })
    .await?
}

async fn items_one(
    path: &Path,
    registry: &HandlerRegistry,
    selection: &Selection,
) -> CliResult<Value> {
    let file = fs::File::open(path).await?;
    let asset = asset::Asset::open_with(registry, file, format_hint(path).as_deref()).await?;
    let (structure, mut source) = asset.into_parts();
    let items = describe_items(&structure, &mut source, selection).await?;
    Ok(json!({
        "schema_version": 1,
        "status": "ok",
        "operation": "items",
        "path": path.display().to_string(),
        "container": {
            "id": structure.container_id(),
            "media_type": structure.media_type(),
        },
        "size_bytes": structure.total_size(),
        "item_count": items.len(),
        "items": items,
    }))
}

async fn describe_items(
    structure: &Structure,
    source: &mut fs::File,
    selection: &Selection,
) -> CliResult<Vec<Value>> {
    let mut items = Vec::new();
    for (index, item) in selection.select(structure)? {
        let ty = match item.jumbf_type() {
            Some(ty) => Some(ty),
            None if item.kind() == ItemKind::Jumbf => {
                asset::run(
                    asset_io::task::Task::new(structure.total_size(), |mut io| async move {
                        structure.jumbf_type_async(&mut io, item).await
                    }),
                    source,
                )
                .await?
            }
            None => None,
        };
        items.push(item_json(index, item, ty));
    }
    Ok(items)
}

fn structure_json(path: &Path, structure: &Structure, items: Vec<Value>) -> Value {
    let segments: Vec<_> = structure
        .segments()
        .iter()
        .enumerate()
        .map(|(index, segment)| segment_json(structure, index, segment))
        .collect();
    let subassets: Vec<_> = structure
        .subassets()
        .iter()
        .enumerate()
        .map(|(index, subasset)| {
            json!({
                "index": index,
                "range": range_json(subasset.range()),
                "media_type": subasset.media_type(),
            })
        })
        .collect();

    json!({
        "schema_version": 1,
        "status": "ok",
        "operation": "inspect",
        "path": path.display().to_string(),
        "container": {
            "id": structure.container_id(),
            "media_type": structure.media_type(),
        },
        "size_bytes": structure.total_size(),
        "hash_binding": structure.c2pa_hash().map(|hash| match hash {
            C2paHash::Data => "data",
            C2paHash::Box => "boxes",
            C2paHash::Bmff => "bmff",
            _ => "unknown",
        }),
        "segment_count": segments.len(),
        "item_count": items.len(),
        "segments": segments,
        "items": items,
        "subassets": subassets,
    })
}

fn segment_json(structure: &Structure, index: usize, segment: &Segment) -> Value {
    let name = segment.name().to_string();
    let box_name = structure
        .box_name(segment.name())
        .map(|value| value.into_owned());
    let roles: Vec<_> = segment
        .regions()
        .map(|(role, range)| {
            json!({
                "role": format!("{role:?}").to_ascii_lowercase(),
                "range": range_json(range),
            })
        })
        .collect();
    let item_indices: Vec<_> = structure
        .items()
        .enumerate()
        .filter_map(|(i, item)| {
            item.extents()
                .any(|extent| extent.segment().index() == index)
                .then_some(i)
        })
        .collect();

    json!({
        "index": index,
        "kind": segment.kind().as_str(),
        "name": if name.is_empty() { None } else { Some(name) },
        "box_name": box_name,
        "range": range_json(segment.range()),
        "header_bytes": segment.header().len(),
        "payload_bytes": segment.payload().len(),
        "trailer_bytes": segment.trailer().len(),
        "subdivides": segment.subdivides(),
        "items": item_indices,
        "regions": roles,
    })
}

fn item_json(index: usize, item: &Item, jumbf_type: Option<[u8; 16]>) -> Value {
    let mut value = item_details_json(item, jumbf_type);
    value["index"] = json!(index);
    value
}

fn item_details_json(item: &Item, jumbf_type: Option<[u8; 16]>) -> Value {
    let extents: Vec<_> = item
        .extents()
        .map(|extent| {
            json!({
                "segment": extent.segment().index(),
                "range": range_json(extent.range()),
            })
        })
        .collect();
    let relations: Vec<_> = item
        .relations()
        .iter()
        .map(|relation| {
            json!({
                "kind": relation.kind().as_str(),
                "native": String::from_utf8_lossy(&relation.native()).to_string(),
                "target": relation.target(),
            })
        })
        .collect();
    let native_type = item.native_type().and_then(|name| {
        if name.is_none() {
            None
        } else {
            Some(name.to_string())
        }
    });

    json!({
        "kind": item.kind().as_str(),
        "content_type": if item.kind() == ItemKind::Jumbf
            && jumbf_type == Some(asset_io::jumbf::C2PA_STORE_TYPE) {
            Some("c2pa_manifest_store")
        } else {
            None
        },
        "category": item.kind().category().as_str(),
        "encoding": item.encoding().as_str(),
        "transfer": transfer_json(item.transfer()),
        "label": item.label(),
        "media_type": item.media_type(),
        "jumbf_type": jumbf_type.map(|ty| hex(&ty)),
        "native_type": native_type,
        "native_id": item.native_id(),
        "uri": item.uri(),
        "size_bytes": item.len(),
        "extent_count": item.extent_count(),
        "extents": extents,
        "relations": relations,
    })
}

fn transfer_json(transfer: &TransferEncoding) -> Value {
    match transfer {
        TransferEncoding::Zlib {
            png_predictor_columns,
        } => json!({
            "kind": "zlib",
            "png_predictor_columns": png_predictor_columns,
        }),
        TransferEncoding::Unknown(label) => json!({
            "kind": "unknown",
            "label": label,
        }),
        other => json!({ "kind": other.to_string() }),
    }
}

fn range_json(range: asset_io::ByteRange) -> Value {
    json!({
        "offset": range.offset(),
        "length": range.len(),
        "end": range.end(),
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[allow(clippy::too_many_arguments)]
async fn extract_one(
    path: &Path,
    asset_index: usize,
    output_dir: Option<&Path>,
    output: Option<&Path>,
    selection: &Selection,
    stored: bool,
    force: bool,
    inputs: &[PathBuf],
    registry: &HandlerRegistry,
) -> CliResult<Value> {
    let file = fs::File::open(path).await?;
    let asset = asset::Asset::open_with(registry, file, format_hint(path).as_deref()).await?;
    let selected = selection.select(asset.structure())?;
    if selected.is_empty() || (output.is_some() && selected.len() != 1) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            if selected.is_empty() {
                "no items match the selection"
            } else {
                "--output requires exactly one selected item; use --item, --kind, or --label"
            },
        )
        .into());
    }
    let folder = output_dir.map(|dir| {
        dir.join(format!(
            "{asset_index:03}-{}",
            safe_component(path.file_name().unwrap_or_default())
        ))
    });
    let mut destinations = Vec::with_capacity(selected.len());
    for (index, item) in &selected {
        let filename = format!(
            "{index:03}-{}{}.{}",
            item.kind().as_str(),
            item.label()
                .map(|label| format!("-{}", safe_component(OsStr::new(label))))
                .unwrap_or_default(),
            item_extension(item)
        );
        let destination = match (output, &folder) {
            (Some(output), _) => output.to_path_buf(),
            (_, Some(folder)) => folder.join(filename),
            _ => unreachable!("clap requires an output destination"),
        };
        prepare_destination(inputs, &destination, force).await?;
        destinations.push(destination);
    }
    let mut extracted = Vec::with_capacity(selected.len());
    for ((index, item), destination) in selected.into_iter().zip(destinations) {
        let bytes = if stored {
            asset.read_item(item).await?
        } else {
            asset.read_content(item).await?
        };
        let mut out = AtomicOutput::create(&destination).await?;
        out.file.write_all(&bytes).await?;
        out.publish(&destination, force).await?;
        extracted.push(json!({
            "item": index,
            "kind": item.kind().as_str(),
            "representation": if stored { "stored" } else { "content" },
            "bytes": bytes.len(),
            "output": destination.display().to_string(),
        }));
    }
    Ok(json!({
        "schema_version": 1,
        "status": "ok",
        "operation": "extract",
        "path": path.display().to_string(),
        "output_dir": folder.map(|folder| folder.display().to_string()),
        "extracted": extracted,
    }))
}

fn item_extension(item: &Item) -> String {
    match item.kind() {
        ItemKind::Xmp => "xmp".to_owned(),
        ItemKind::Jumbf => "jumbf".to_owned(),
        ItemKind::Exif => "exif".to_owned(),
        _ => item
            .media_type()
            .and_then(|media_type| media_type.split('/').nth(1))
            .filter(|subtype| !subtype.is_empty())
            .map(|subtype| safe_component(OsStr::new(subtype)))
            .unwrap_or_else(|| "bin".to_owned()),
    }
}

fn safe_component(value: &OsStr) -> String {
    let text = value.to_string_lossy();
    let safe: String = text
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect();
    if safe.is_empty() || safe == "." || safe == ".." {
        "item".to_owned()
    } else {
        safe
    }
}

#[allow(clippy::too_many_arguments)]
async fn update_one(
    input: &Path,
    index: usize,
    input_count: usize,
    output: Option<&Path>,
    output_dir: Option<&Path>,
    xmp_file: Option<&Path>,
    remove_xmp: bool,
    jumbf_file: Option<&Path>,
    remove_jumbf: bool,
    force: bool,
    inputs: &[PathBuf],
    registry: &HandlerRegistry,
) -> CliResult<Value> {
    let xmp = match xmp_file {
        Some(path) => Some(fs::read(path).await?),
        None => None,
    };
    let jumbf = match jumbf_file {
        Some(path) => Some(fs::read(path).await?),
        None => None,
    };
    let mut updates = Updates::new();
    if let Some(bytes) = xmp {
        updates = updates.set_xmp(bytes);
    } else if remove_xmp {
        updates = updates.remove_xmp();
    }
    if let Some(bytes) = jumbf {
        updates = updates.set_jumbf(bytes);
    } else if remove_jumbf {
        updates = updates.remove_jumbf();
    }

    let destination = match (output, output_dir) {
        (Some(path), None) => path.to_path_buf(),
        (None, Some(dir)) => dir.join(format!(
            "{index:03}-{}",
            safe_component(input.file_name().unwrap_or_default())
        )),
        (None, None) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "specify --output for one input or --output-dir for multiple inputs",
            )
            .into())
        }
        (Some(_), Some(_)) => unreachable!("clap prevents specifying both output options"),
    };
    if input_count > 1 && output.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--output is only valid for a single matched input; use --output-dir",
        )
        .into());
    }
    if xmp_file.is_none() && !remove_xmp && jumbf_file.is_none() && !remove_jumbf {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "specify an XMP/JUMBF file or a removal option",
        )
        .into());
    }
    prepare_destination(inputs, &destination, force).await?;

    let src = fs::File::open(input).await?;
    let asset = asset::Asset::open_with(registry, src, format_hint(input).as_deref()).await?;
    let mut out = AtomicOutput::create(&destination).await?;
    let written = asset
        .write(&mut out.file, &updates, &mut asset_io::NullSink)
        .await?;
    out.publish(&destination, force).await?;
    Ok(json!({
        "schema_version": 1,
        "status": "ok",
        "operation": "update",
        "path": input.display().to_string(),
        "output": destination.display().to_string(),
        "size_bytes": written.total_size(),
        "container": written.container_id(),
    }))
}

fn output_parent(output: &Path) -> &Path {
    output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

async fn prepare_destination(inputs: &[PathBuf], output: &Path, force: bool) -> CliResult<()> {
    let filename = output.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "output path has no file name")
    })?;
    fs::create_dir_all(output_parent(output)).await?;
    let resolved = fs::canonicalize(output_parent(output))
        .await?
        .join(filename);
    let metadata = match fs::symlink_metadata(output).await {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    for input in inputs {
        let mut same = fs::canonicalize(input).await? == resolved;
        #[cfg(unix)]
        if let Some(output_metadata) = &metadata {
            use std::os::unix::fs::MetadataExt;
            let input_metadata = fs::metadata(input).await?;
            same |= input_metadata.dev() == output_metadata.dev()
                && input_metadata.ino() == output_metadata.ino();
        }
        if same {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "input and output paths must be different; --force never overwrites an input",
            )
            .into());
        }
    }
    if let Some(metadata) = metadata {
        if !metadata.is_file() || metadata.is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "output must be a regular file, not a directory or symlink: {}",
                    output.display()
                ),
            )
            .into());
        }
        if !force {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "output exists: {}; use --force to replace it",
                    output.display()
                ),
            )
            .into());
        }
    }
    Ok(())
}

struct AtomicOutput {
    file: fs::File,
    temporary: tempfile::NamedTempFile,
}

impl AtomicOutput {
    async fn create(destination: &Path) -> CliResult<Self> {
        let parent = output_parent(destination).to_path_buf();
        let temporary =
            tokio::task::spawn_blocking(move || tempfile::NamedTempFile::new_in(parent)).await??;
        let file = fs::File::from_std(temporary.reopen()?);
        Ok(Self { file, temporary })
    }

    async fn publish(mut self, destination: &Path, force: bool) -> CliResult<()> {
        self.file.flush().await?;
        self.file.sync_all().await?;
        drop(self.file);
        let destination = destination.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let published = if force {
                self.temporary.persist(destination)
            } else {
                self.temporary.persist_noclobber(destination)
            };
            published.map(|_| ()).map_err(|error| error.error)
        })
        .await??;
        Ok(())
    }
}

async fn embedded_one(path: PathBuf, limits: ScanLimits) -> CliResult<Value> {
    tokio::task::spawn_blocking(move || {
        let registry = HandlerRegistry::builtin();
        let mut file = std::fs::File::open(&path)?;
        let structure = Structure::parse(&registry, &mut file, format_hint(&path).as_deref())?;
        let children = embedded_json(&structure, &mut file, &registry, limits)?;
        Ok::<_, Box<dyn Error + Send + Sync>>(json!({
            "schema_version": 1,
            "status": "ok",
            "operation": "embedded",
            "path": path.display().to_string(),
            "embedded_count": children.len(),
            "embedded": children,
        }))
    })
    .await?
}

fn embedded_json(
    structure: &Structure,
    file: &mut std::fs::File,
    registry: &HandlerRegistry,
    limits: ScanLimits,
) -> CliResult<Vec<Value>> {
    let embedded = structure.scan_embedded(file, registry, limits)?;
    embedded
        .iter()
        .enumerate()
        .map(|(index, child)| {
            let parent_structure = match child.parent {
                Some(parent) => embedded[parent].structure.as_ref().map_err(|error| {
                    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
                })?,
                None => structure,
            };
            let source_item_index = parent_structure
                .items()
                .position(|item| {
                    item.kind() == child.item.kind()
                        && item.native_id() == child.item.native_id()
                        && item
                            .extents()
                            .map(|extent| (extent.segment(), extent.range()))
                            .eq(child
                                .item
                                .extents()
                                .map(|extent| (extent.segment(), extent.range())))
                })
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "embedded source item is missing from its parent structure",
                    )
                })?;
            let parsed = match &child.structure {
                Ok(structure) => {
                    let mut reader = child.reader(file)?;
                    let items = structure
                        .items()
                        .enumerate()
                        .map(|(item_index, item)| {
                            let ty = structure.jumbf_type(&mut reader, item)?;
                            Ok(item_json(item_index, item, ty))
                        })
                        .collect::<asset_io::Result<Vec<_>>>()?;
                    json!({
                        "status": "parsed",
                        "container": structure.container_id(),
                        "media_type": structure.media_type(),
                        "size_bytes": structure.total_size(),
                        "item_count": structure.items().count(),
                        "items": items,
                    })
                }
                Err(error) => json!({
                    "status": "not_parsed",
                    "error": error.to_string(),
                }),
            };
            Ok::<_, Box<dyn Error + Send + Sync>>(json!({
                "index": index,
                "parent": child.parent,
                "depth": child.depth,
                "source_item_index": source_item_index,
                "source_item": item_details_json(&child.item, child.item.jumbf_type()),
                "ranges_in_parent": child.ranges()
                    .iter()
                    .map(|range| range_json(*range))
                    .collect::<Vec<_>>(),
                "asset": parsed,
            }))
        })
        .collect()
}

fn print_human(command: &Command, results: &[Value]) {
    for result in results {
        if let Some(error) = result.get("error").and_then(Value::as_str) {
            println!(
                "{}: error: {error}",
                result["path"].as_str().unwrap_or("<input>")
            );
            continue;
        }
        let path = result["path"].as_str().unwrap_or("<input>");
        match command {
            Command::Inspect { segments, .. } => {
                println!(
                    "{}: {} ({}) {} bytes; {} segments; {} items",
                    path,
                    result["container"]["media_type"]
                        .as_str()
                        .unwrap_or("unknown"),
                    result["container"]["id"].as_str().unwrap_or("unknown"),
                    result["size_bytes"].as_u64().unwrap_or_default(),
                    result["segment_count"].as_u64().unwrap_or_default(),
                    result["item_count"].as_u64().unwrap_or_default(),
                );
                if *segments {
                    println!("\nSegments (non-overlapping; sizes in bytes):");
                    let mut rows = Vec::new();
                    if let Some(segments) = result["segments"].as_array() {
                        for segment in segments {
                            rows.push([
                                segment["index"].to_string(),
                                element_name(segment),
                                text(&segment["kind"]),
                                segment["range"]["offset"].to_string(),
                                segment["range"]["length"].to_string(),
                                segment["header_bytes"].to_string(),
                                segment["payload_bytes"].to_string(),
                                segment["trailer_bytes"].to_string(),
                            ]);
                        }
                    }
                    for line in format_table(
                        [
                            "Index", "Name", "Kind", "Offset", "Size", "Header", "Payload",
                            "Trailer",
                        ],
                        &rows,
                        &[0, 3, 4, 5, 6, 7],
                    ) {
                        println!("{line}");
                    }
                } else {
                    println!("\nStructure (parent sizes include children):");
                    let mut rows = Vec::new();
                    if let Some(nodes) = result["nodes"].as_array() {
                        for node in nodes {
                            let depth = node["depth"].as_u64().unwrap_or(1);
                            rows.push([
                                node["range"]["offset"].to_string(),
                                size_display(node["range"]["length"].as_u64().unwrap_or_default()),
                                format!(
                                    "{}{}",
                                    "  ".repeat(depth.saturating_sub(1).min(64) as usize),
                                    element_name(node)
                                ),
                                text(&node["path"]),
                            ]);
                        }
                    }
                    for line in format_table(["Offset", "Size", "Element", "Path"], &rows, &[0, 1])
                    {
                        println!("{line}");
                    }
                }
                print_items(result);
                print_embedded(result);
            }
            Command::Items { .. } => {
                println!(
                    "{path}: {} ({}) {} bytes; {} items",
                    text(&result["container"]["media_type"]),
                    text(&result["container"]["id"]),
                    result["size_bytes"],
                    result["item_count"],
                );
                print_items(result);
            }
            Command::Extract { .. } => {
                println!(
                    "{path}: extracted {} item(s)",
                    result["extracted"].as_array().map_or(0, Vec::len)
                );
                if let Some(items) = result["extracted"].as_array() {
                    for item in items {
                        println!(
                            "  [{}] {} bytes -> {}",
                            item["item"],
                            item["bytes"],
                            text(&item["output"])
                        );
                    }
                }
            }
            Command::Update { .. } => println!(
                "{path}: wrote {} bytes to {}",
                result["size_bytes"].as_u64().unwrap_or_default(),
                result["output"].as_str().unwrap_or("")
            ),
            Command::Embedded { .. } => {
                println!(
                    "{path}: {} embedded asset(s)",
                    result["embedded_count"].as_u64().unwrap_or_default()
                );
                print_embedded(result);
            }
        }
    }
}

fn print_embedded(result: &Value) {
    if let Some(embedded) = result["embedded"]
        .as_array()
        .filter(|assets| !assets.is_empty())
    {
        println!("\nEmbedded assets:");
        for child in embedded {
            let asset = &child["asset"];
            let parent = child["parent"]
                .as_u64()
                .map(|index| format!("embedded [{index}]"))
                .unwrap_or_else(|| "root".to_owned());
            println!(
                "\n  Embedded [{}] from {parent} item [{}] (depth {}):",
                child["index"], child["source_item_index"], child["depth"]
            );
            if let Some(id) = child["source_item"]["native_id"].as_u64() {
                println!("    native ID: {id}");
            }
            if !child["source_item"]["uri"].is_null() {
                println!("    uri: {}", text(&child["source_item"]["uri"]));
            }
            if asset["status"] == "parsed" {
                println!(
                    "    {} ({}), {}, {} items",
                    text(&asset["media_type"]),
                    text(&asset["container"]),
                    size_display(asset["size_bytes"].as_u64().unwrap_or_default()),
                    asset["item_count"]
                );
                print_items(asset);
            } else {
                println!("    not parsed: {}", text(&asset["error"]));
            }
        }
    }
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or("-").escape_debug().to_string()
}

fn element_name(element: &Value) -> String {
    let raw = text(&element["name"]);
    match element["box_name"].as_str() {
        Some(name) if !name.is_empty() && name != element["name"].as_str().unwrap_or("") => {
            format!("{} ({raw})", name.escape_debug())
        }
        _ => raw,
    }
}

fn size_display(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut size = bytes as f64;
    let mut unit = "B";
    for next in ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"] {
        size /= 1024.0;
        unit = next;
        if size < 1024.0 {
            break;
        }
    }
    format!("{size:.1} {unit} ({bytes} B)")
}

fn print_items(result: &Value) {
    println!("\nItems (stored sizes; indexes are local to this file):");
    let items = result["items"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let rows: Vec<_> = items
        .iter()
        .map(|item| {
            [
                item["index"].to_string(),
                text(&item["kind"]),
                if item["content_type"] == "c2pa_manifest_store" {
                    "C2PA".to_owned()
                } else {
                    "-".to_owned()
                },
                text(&item["encoding"]),
                text(&item["transfer"]["kind"]),
                size_display(item["size_bytes"].as_u64().unwrap_or_default()),
                item["extent_count"].to_string(),
                item["native_id"]
                    .as_u64()
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "-".into()),
                text(&item["label"]),
            ]
        })
        .collect();
    let lines = format_table(
        [
            "Index",
            "Kind",
            "Content",
            "Encoding",
            "Transfer",
            "Size",
            "Extents",
            "Native ID",
            "Label",
        ],
        &rows,
        &[0, 5, 6, 7],
    );
    println!("{}", lines[0]);
    for (item, line) in items.iter().zip(&lines[1..]) {
        println!("{line}");
        for key in ["native_type", "media_type", "uri", "jumbf_type"] {
            if !item[key].is_null() {
                println!("        {key}: {}", text(&item[key]));
            }
        }
        if let Some(relations) = item["relations"].as_array() {
            for relation in relations {
                println!(
                    "        {} ({}) -> native ID {}",
                    text(&relation["kind"]),
                    text(&relation["native"]),
                    relation["target"]
                );
            }
        }
    }
    if items.is_empty() {
        println!("  (none)");
    }
}

fn format_table<const N: usize>(
    headers: [&str; N],
    rows: &[[String; N]],
    numeric_columns: &[usize],
) -> Vec<String> {
    let headers = headers.map(str::to_owned);
    let widths: [usize; N] = std::array::from_fn(|column| {
        std::iter::once(&headers)
            .chain(rows)
            .map(|row| row[column].chars().count())
            .max()
            .unwrap_or_default()
    });
    std::iter::once(&headers)
        .chain(rows)
        .map(|row| {
            let cells: Vec<_> = row
                .iter()
                .enumerate()
                .map(|(column, value)| {
                    let width = widths[column];
                    if numeric_columns.contains(&column) {
                        format!("{value:>width$}")
                    } else {
                        format!("{value:<width$}")
                    }
                })
                .collect();
            format!("  {}", cells.join("  ").trim_end())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn element_names_keep_raw_identifiers_without_repeating_identical_names() {
        assert_eq!(
            element_name(&json!({"name": "0xFFD8", "box_name": "SOI"})),
            "SOI (0xFFD8)"
        );
        assert_eq!(
            element_name(&json!({"name": "moov", "box_name": "moov"})),
            "moov"
        );
        assert_eq!(
            element_name(&json!({"name": "unknown", "box_name": null})),
            "unknown"
        );
    }

    #[test]
    fn table_columns_share_widths_and_right_align_numbers() {
        let rows = [
            ["0", "xmp", "xml", "identity", "6 B", "1", "-", "-"].map(str::to_owned),
            ["1", "jumbf", "jumbf", "identity", "8 B", "1", "-", "-"].map(str::to_owned),
        ];
        let lines = format_table(
            [
                "Index",
                "Kind",
                "Encoding",
                "Transfer",
                "Size",
                "Extents",
                "Native ID",
                "Label",
            ],
            &rows,
            &[0, 4, 5, 6],
        );
        assert_eq!(
            lines,
            [
                "  Index  Kind   Encoding  Transfer  Size  Extents  Native ID  Label",
                "      0  xmp    xml       identity   6 B        1          -  -",
                "      1  jumbf  jumbf     identity   8 B        1          -  -",
            ]
        );
        let rows = [
            ["0", "6 B", "xmp"].map(str::to_owned),
            [
                "123456",
                "16.0 EiB (18446744073709551615 B)",
                "embedded_asset",
            ]
            .map(str::to_owned),
        ];
        let lines = format_table(["Index", "Size", "Kind"], &rows, &[0, 1]);
        let kind_column = lines[0].find("Kind").unwrap();
        assert_eq!(lines[1].find("xmp"), Some(kind_column));
        assert_eq!(lines[2].find("embedded_asset"), Some(kind_column));
        assert_eq!(
            lines[0].find("Size").unwrap() + 4,
            lines[1].find("6 B").unwrap() + 3
        );
    }

    #[tokio::test]
    async fn atomic_output_refuses_a_destination_created_during_the_write() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("output");
        let mut output = AtomicOutput::create(&destination).await.unwrap();
        output.file.write_all(b"new").await.unwrap();
        fs::write(&destination, b"existing").await.unwrap();
        assert!(output.publish(&destination, false).await.is_err());
        assert_eq!(fs::read(&destination).await.unwrap(), b"existing");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn abandoned_atomic_output_preserves_the_destination_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("output");
        fs::write(&destination, b"existing").await.unwrap();
        let mut output = AtomicOutput::create(&destination).await.unwrap();
        output.file.write_all(b"partial").await.unwrap();
        output.file.flush().await.unwrap();
        drop(output);
        assert_eq!(fs::read(&destination).await.unwrap(), b"existing");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn cli_has_expected_commands_and_output_flags() {
        let cli = Cli::try_parse_from(["aio", "inspect", "image.jpg"]).unwrap();
        assert!(!cli.human);
        assert!(!cli.json);
        assert!(matches!(cli.command, Command::Inspect { .. }));
        assert!(Cli::try_parse_from(["aio", "--human", "--json", "items", "image.jpg"]).is_err());
    }

    #[test]
    fn safe_components_cannot_escape_output_directories() {
        assert_eq!(safe_component(OsStr::new("../secret")), "..-secret");
        assert_eq!(safe_component(OsStr::new("..")), "item");
    }

    #[test]
    fn glob_expansion_is_sorted_and_deduplicated() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.svg"), "<svg/>").unwrap();
        std::fs::write(dir.path().join("a.svg"), "<svg/>").unwrap();
        let pattern = dir.path().join("*.svg").display().to_string();
        let paths = expand_inputs(&[pattern.clone(), pattern]).unwrap();

        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0].file_name().unwrap(), OsStr::new("a.svg"));
        assert_eq!(paths[1].file_name().unwrap(), OsStr::new("b.svg"));
    }

    #[test]
    fn clap_rejects_conflicting_update_actions() {
        assert!(Cli::try_parse_from([
            "aio",
            "update",
            "in.jpg",
            "--xmp-file",
            "xmp.xml",
            "--remove-xmp"
        ])
        .is_err());
    }
}
