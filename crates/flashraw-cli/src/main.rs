//! `flashraw` command-line tool.

mod output;

use clap::{Args, Parser, Subcommand};
use flashraw::{Backend, CorruptionPolicy, DecodeOptions, Demosaic, FlashRaw, OutputFormat, Rect};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "flashraw", version, about = "Fast RAW image decoder", propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone)]
struct DecodeArgs {
    /// Backend: auto, cpu, simd, gpu.
    #[arg(long, default_value = "auto")]
    backend: String,
    /// Output: raw-u16, raw-f32, normalized, linear-rgb, linear-rgb16, linear-rgb-f16, rgba8,
    /// rgb16.
    #[arg(long, default_value = "raw-u16")]
    output: String,
    /// Decode only a region: x,y,width,height (sensor coordinates).
    #[arg(long, value_parser = parse_rect)]
    region: Option<Rect>,
    /// Decode a single tile/strip by index.
    #[arg(long)]
    tile: Option<usize>,
    /// Worker threads (default: all cores).
    #[arg(long)]
    threads: Option<usize>,
    /// Demosaic for RGB outputs: bilinear, half.
    #[arg(long, default_value = "bilinear")]
    demosaic: String,
    /// Do not apply DNG opcode lists (bad pixels, lens shading, ...).
    #[arg(long)]
    no_opcodes: bool,
    /// RGB outputs: exposure in EV, applied to linear values before encoding.
    #[arg(long, default_value_t = 0.0, allow_negative_numbers = true)]
    exposure: f32,
    /// RGB outputs: linear value mapped to black (after exposure).
    #[arg(long, default_value_t = 0.0, allow_negative_numbers = true)]
    black_point: f32,
    /// RGB outputs: linear value mapped to white (after exposure).
    #[arg(long, default_value_t = 1.0)]
    white_point: f32,
    /// Fail on truncated or corrupt sensor data instead of zero-filling what is missing
    /// (with a warning, the default).
    #[arg(long)]
    strict: bool,
    /// Print per-stage timings.
    #[arg(long)]
    profile: bool,
    /// Machine-readable output.
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show a summary of a RAW file.
    Info {
        files: Vec<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Print all metadata as JSON.
    DumpMetadata { file: PathBuf },
    /// Decode sensor data (or RGB) to a file. Format from --format or extension:
    /// .raw (u16 LE samples; half-float bits for linear-rgb-f16), .pgm/.ppm (16-bit Netpbm),
    /// .tif/.tiff (16-bit), .jpg (rgba8).
    Decode {
        file: PathBuf,
        out: PathBuf,
        #[arg(long)]
        format: Option<String>,
        #[command(flatten)]
        args: DecodeArgs,
    },
    /// Extract the largest embedded preview.
    Preview { file: PathBuf, out: PathBuf },
    /// Extract the smallest embedded preview (thumbnail).
    Thumbnail { file: PathBuf, out: PathBuf },
    /// Benchmark decoding.
    Benchmark {
        files: Vec<PathBuf>,
        #[arg(long, default_value_t = 5)]
        iterations: usize,
        #[command(flatten)]
        args: DecodeArgs,
    },
    /// Decode and print hashes and geometry of the decoded data (for regression testing).
    Verify {
        files: Vec<PathBuf>,
        #[arg(long)]
        json: bool,
        /// Also write raw u16 samples to <file>.flashraw.u16 next to the output dir.
        #[arg(long)]
        dump_dir: Option<PathBuf>,
        /// Hash the samples with DNG opcode lists applied (the default decode). Without
        /// it, samples are hashed as stored (the payload decode alone).
        #[arg(long)]
        apply_opcodes: bool,
        /// Fail on truncated or corrupt sensor data instead of recovering it (recovered
        /// files report `incomplete` and `warnings`).
        #[arg(long)]
        strict: bool,
    },
    /// List supported formats and compression modes.
    Supported {
        #[arg(long)]
        json: bool,
    },
    /// Camera database tooling.
    #[command(subcommand)]
    CameraDb(CameraDbCmd),
}

#[derive(Subcommand)]
enum CameraDbCmd {
    /// Validate the database file.
    Validate {
        #[arg(default_value = "camera-db/cameras.json")]
        db: PathBuf,
    },
    /// Re-normalise (load, sort and rewrite) the database file.
    Generate {
        #[arg(default_value = "camera-db/cameras.json")]
        db: PathBuf,
    },
    /// Compare two database files.
    Compare { a: PathBuf, b: PathBuf },
    /// Show statistics of the compiled-in database.
    Stats,
}

fn parse_rect(s: &str) -> Result<Rect, String> {
    let v: Vec<u32> =
        s.split(',').map(|x| x.trim().parse().map_err(|_| format!("bad number in {s:?}"))).collect::<Result<_, _>>()?;
    if v.len() != 4 {
        return Err("expected x,y,width,height".into());
    }
    Ok(Rect::new(v[0], v[1], v[2], v[3]))
}

fn policy(strict: bool) -> CorruptionPolicy {
    if strict { CorruptionPolicy::Strict } else { CorruptionPolicy::Recover }
}

fn decode_options(a: &DecodeArgs) -> Result<DecodeOptions, String> {
    Ok(DecodeOptions {
        backend: Backend::parse(&a.backend).ok_or_else(|| format!("unknown backend {:?}", a.backend))?,
        output: OutputFormat::parse(&a.output).ok_or_else(|| format!("unknown output {:?}", a.output))?,
        threads: a.threads,
        region: a.region,
        demosaic: if a.demosaic.starts_with("half") { Demosaic::HalfSize } else { Demosaic::Bilinear },
        apply_opcodes: !a.no_opcodes,
        corruption_policy: policy(a.strict),
        exposure: a.exposure,
        black_point: a.black_point,
        white_point: a.white_point,
        ..Default::default()
    })
}

fn main() -> ExitCode {
    // Behave like a normal Unix tool when piped into `head` etc.: exit quietly on
    // EPIPE instead of panicking in `println!`.
    #[cfg(unix)]
    // SAFETY: restoring the default SIGPIPE disposition at startup, before any threads exist.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

fn open(path: &PathBuf) -> Result<FlashRaw<'static>, String> {
    FlashRaw::open_path(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn run(cli: Cli) -> Result<ExitCode, String> {
    match cli.cmd {
        Cmd::Info { files, json } => {
            let mut failed = false;
            for f in &files {
                match open(f) {
                    Ok(raw) => {
                        if json {
                            println!("{}", output::info_json(f, &raw));
                        } else {
                            output::print_info(f, &raw);
                        }
                    }
                    Err(e) => {
                        failed = true;
                        if json {
                            println!("{}", serde_json::json!({"file": f, "error": e}));
                        } else {
                            eprintln!("{e}");
                        }
                    }
                }
            }
            Ok(if failed { ExitCode::from(2) } else { ExitCode::SUCCESS })
        }
        Cmd::DumpMetadata { file } => {
            let raw = open(&file)?;
            let v = serde_json::json!({
                "metadata": raw.metadata(),
                "frame": output::frame_info_json(raw.frame_info()),
                "capabilities": raw.capabilities(),
                "units": raw.units().len(),
            });
            println!("{}", serde_json::to_string_pretty(&v).unwrap());
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Decode { file, out, format, args } => {
            let raw = open(&file)?;
            let opts = decode_options(&args)?;
            let frame = match args.tile {
                Some(t) => raw.decode_tile(t, opts),
                None => raw.decode(opts),
            }
            .map_err(|e| format!("{}: {e}", file.display()))?;
            for w in frame.warnings() {
                eprintln!("warning: {}: {w}", file.display());
            }
            let fmt = format
                .or_else(|| out.extension().map(|e| e.to_string_lossy().to_lowercase()))
                .unwrap_or_else(|| "raw".into());
            output::write_frame(&frame, &out, &fmt)?;
            if args.profile {
                output::print_profile(&frame);
            }
            if args.json {
                println!(
                    "{}",
                    serde_json::json!({"width": frame.width, "height": frame.height, "cpp": frame.cpp, "stats": frame.stats,
                        "incomplete": frame.is_incomplete(), "warnings": output::warnings_json(frame.warnings())})
                );
            } else {
                eprintln!("wrote {} ({}x{}x{})", out.display(), frame.width, frame.height, frame.cpp);
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Preview { file, out } => {
            let raw = open(&file)?;
            let p = raw.preview().cloned().ok_or("file has no embedded preview")?;
            output::write_preview(&raw, &p, &out)?;
            eprintln!("wrote {} ({}x{} {:?})", out.display(), p.width, p.height, p.encoding);
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Thumbnail { file, out } => {
            let raw = open(&file)?;
            let p = raw.thumbnail().cloned().ok_or("file has no embedded thumbnail")?;
            output::write_preview(&raw, &p, &out)?;
            eprintln!("wrote {} ({}x{} {:?})", out.display(), p.width, p.height, p.encoding);
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Benchmark { files, iterations, args } => {
            let opts = decode_options(&args)?;
            for f in &files {
                output::benchmark(f, &opts, iterations.max(1), args.json)?;
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Verify { files, json, dump_dir, apply_opcodes, strict } => {
            let mut failed = false;
            for f in &files {
                let r = output::verify(f, dump_dir.as_deref(), apply_opcodes, policy(strict));
                if r.get("error").is_some() {
                    failed = true;
                }
                if json {
                    println!("{r}");
                } else {
                    println!("{}", serde_json::to_string_pretty(&r).unwrap());
                }
            }
            Ok(if failed { ExitCode::from(2) } else { ExitCode::SUCCESS })
        }
        Cmd::Supported { json } => {
            output::supported(json);
            Ok(ExitCode::SUCCESS)
        }
        Cmd::CameraDb(c) => camera_db(c),
    }
}

fn camera_db(c: CameraDbCmd) -> Result<ExitCode, String> {
    use flashraw_camera_db::tooling::*;
    match c {
        CameraDbCmd::Validate { db } => {
            let d = load_db(&db)?;
            let problems = validate(&d);
            for p in &problems {
                println!("{p}");
            }
            println!(
                "{} cameras, {} fallback matrices, {} alias groups, {} size rules, {} problems",
                d.cameras.len(),
                d.matrices.len(),
                d.model_aliases.len(),
                d.size_rules.len(),
                problems.len()
            );
            Ok(if problems.is_empty() { ExitCode::SUCCESS } else { ExitCode::from(1) })
        }
        CameraDbCmd::Generate { db } => {
            let d = load_db(&db)?;
            write_db(&db, &d)?;
            println!(
                "rewrote {} ({} cameras); rebuild to regenerate the compiled tables",
                db.display(),
                d.cameras.len()
            );
            Ok(ExitCode::SUCCESS)
        }
        CameraDbCmd::Compare { a, b } => {
            let diffs = compare(&load_db(&a)?, &load_db(&b)?);
            for d in &diffs {
                println!("{d}");
            }
            println!("{} differences", diffs.len());
            Ok(ExitCode::SUCCESS)
        }
        CameraDbCmd::Stats => {
            let all = flashraw_camera_db::all();
            let mut makes = std::collections::BTreeMap::<&str, usize>::new();
            for c in all {
                *makes.entry(c.canonical_make).or_default() += 1;
            }
            println!(
                "{} entries, {} fallback matrices, {} alias groups, {} size rules",
                all.len(),
                flashraw_camera_db::MATRICES.len(),
                flashraw_camera_db::ALIAS_GROUPS.len(),
                flashraw_camera_db::SIZE_RULES.len()
            );
            for (m, n) in makes {
                println!("  {m:24} {n}");
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}
