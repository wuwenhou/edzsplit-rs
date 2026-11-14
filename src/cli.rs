use std::path::PathBuf;

use crate::archive::Backend;
use crate::commands;
use crate::error::{AppError, Result};
use crate::graph::ClosureMode;
use crate::split::{self, Compression, SplitOptions};
use crate::verify;

pub fn run(args: Vec<String>) -> i32 {
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help" | "help") {
        print_usage();
        return 2;
    }

    if args.len() > 1 {
        // `edzsplit --version` is routed through the command match below.
    }

    let command = args[0].to_ascii_lowercase();
    let rest = &args[1..];
    let outcome = match command.as_str() {
        "inspect" => cmd_inspect(rest),
        "split" => cmd_split(rest),
        "verify" => cmd_verify(rest),
        "case-a" => cmd_case_a(rest),
        "selftest" => crate::selftest::run(),
        "version" | "--version" => {
            println!("edzsplit {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        _ => {
            eprintln!("Unknown command: {}", args[0]);
            print_usage();
            Ok(2)
        }
    };

    match outcome {
        Ok(code) => code,
        Err(error) => {
            if error.is_usage() {
                eprintln!("error: {error}");
                2
            } else {
                eprintln!("fatal: {error}");
                4
            }
        }
    }
}

fn next_value(args: &[String], index: &mut usize, option: &str) -> Result<String> {
    if *index + 1 >= args.len() {
        return Err(AppError::usage(format!("Missing value for {option}")));
    }
    *index += 1;
    Ok(args[*index].clone())
}

fn absolute(path: &str) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path))
}

// ---------------------------------------------------------------- inspect

fn cmd_inspect(args: &[String]) -> Result<i32> {
    let mut json = false;
    let mut backend = Backend::Auto;
    let mut positional: Vec<String> = Vec::new();

    let mut index = 0usize;
    while index < args.len() {
        let argument = &args[index];
        match argument.as_str() {
            "--json" => json = true,
            "--backend" => {
                let value = next_value(args, &mut index, argument)?;
                backend = Backend::parse(&value)?;
            }
            other if other.starts_with("--") => {
                return Err(AppError::usage(format!("Unknown option: {other}")));
            }
            other => positional.push(other.to_string()),
        }
        index += 1;
    }

    if positional.is_empty() {
        return Err(AppError::usage("inspect requires <archive>"));
    }

    commands::inspect(&commands::InspectOptions {
        archive: absolute(&positional[0]),
        json,
        backend,
    })
}

// ---------------------------------------------------------------- split

fn cmd_split(args: &[String]) -> Result<i32> {
    let mut archive: Option<String> = None;
    let mut out_dir: Option<String> = None;
    let mut only: Vec<String> = Vec::new();
    let mut only_from_file: Option<PathBuf> = None;
    let mut closure = ClosureMode::Full;
    let mut fix_refs = "auto".to_string();
    let mut verify_structural = true;
    let mut verify_deep = false;
    let mut resume = false;
    let mut keep_cache = false;
    let mut keep_going = true;
    let mut max_closure_ratio = 1.0f64;
    let mut dry_run = false;
    let mut work_dir: Option<PathBuf> = None;
    let mut log_path: Option<PathBuf> = None;
    let mut backend = Backend::Auto;
    let mut jobs = split::default_jobs();
    let mut compress = Compression::Copy;
    let mut strict_part = false;
    let mut progress = false;

    let mut index = 0usize;
    while index < args.len() {
        let argument = &args[index];
        match argument.as_str() {
            "--out" => out_dir = Some(next_value(args, &mut index, argument)?),
            "--only" => {
                let value = next_value(args, &mut index, argument)?;
                for part in value.split(',') {
                    let part = part.trim();
                    if !part.is_empty() {
                        only.push(part.to_string());
                    }
                }
            }
            "--from-file" => {
                only_from_file = Some(PathBuf::from(next_value(args, &mut index, argument)?));
            }
            "--closure" => {
                let value = next_value(args, &mut index, argument)?;
                closure = ClosureMode::parse(&value)?;
            }
            "--fix-refs" => {
                let value = next_value(args, &mut index, argument)?.to_ascii_lowercase();
                if value != "auto" && value != "never" {
                    return Err(AppError::usage("--fix-refs must be auto or never"));
                }
                fix_refs = value;
            }
            "--verify-structural" => verify_structural = true,
            "--no-verify" => verify_structural = false,
            "--verify-deep" => verify_deep = true,
            "--resume" => resume = true,
            "--keep-cache" => keep_cache = true,
            "--keep-going" => keep_going = true,
            "--fail-fast" => keep_going = false,
            "--max-closure-ratio" => {
                let value = next_value(args, &mut index, argument)?;
                max_closure_ratio = value.parse::<f64>().map_err(|_| {
                    AppError::message(format!("Invalid --max-closure-ratio: {value}"))
                })?;
            }
            "--dry-run" => dry_run = true,
            "--workdir" => {
                work_dir = Some(PathBuf::from(next_value(args, &mut index, argument)?));
            }
            "--log" => {
                log_path = Some(PathBuf::from(next_value(args, &mut index, argument)?));
            }
            "--backend" => {
                let value = next_value(args, &mut index, argument)?;
                backend = Backend::parse(&value)?;
            }
            "--jobs" => {
                let value = next_value(args, &mut index, argument)?;
                jobs = parse_jobs(&value)?;
            }
            "--compress" => {
                let value = next_value(args, &mut index, argument)?;
                compress = Compression::parse(&value)?;
            }
            "--strict-part" => strict_part = true,
            "--progress" => progress = true,
            other if other.starts_with("--") => {
                return Err(AppError::usage(format!("Unknown option: {other}")));
            }
            other => archive = Some(other.to_string()),
        }
        index += 1;
    }

    let archive = archive.ok_or_else(|| AppError::usage("split requires <archive>"))?;
    let out_dir = out_dir.ok_or_else(|| AppError::usage("--out <dir> is required"))?;

    let mut options = SplitOptions::new(absolute(&archive), absolute(&out_dir));
    options.only = only;
    options.only_from_file = only_from_file.map(|path| absolute(&path.to_string_lossy()));
    options.closure = closure;
    options.fix_refs = fix_refs;
    options.verify_structural = verify_structural;
    options.verify_deep = verify_deep;
    options.resume = resume;
    options.keep_cache = keep_cache;
    options.keep_going = keep_going;
    options.max_closure_ratio = max_closure_ratio;
    options.dry_run = dry_run;
    options.work_dir = work_dir.map(|path| absolute(&path.to_string_lossy()));
    options.log_path = log_path.map(|path| absolute(&path.to_string_lossy()));
    options.backend = backend;
    options.jobs = jobs;
    options.compress = compress;
    options.strict_part = strict_part;
    options.progress = progress;

    let report = split::run(&options)?;
    print!("{}", split::split_summary(&report));
    Ok(report.exit_code())
}

fn parse_jobs(value: &str) -> Result<usize> {
    if value.eq_ignore_ascii_case("auto") {
        return Ok(split::default_jobs());
    }
    let jobs: usize = value
        .parse()
        .map_err(|_| AppError::usage("--jobs must be auto or a positive integer"))?;
    if jobs == 0 {
        return Err(AppError::usage("--jobs must be auto or a positive integer"));
    }
    Ok(jobs)
}

// ---------------------------------------------------------------- verify

fn cmd_verify(args: &[String]) -> Result<i32> {
    let mut strict = false;
    let mut deep = true;
    let mut backend = Backend::Auto;
    let mut positional: Vec<String> = Vec::new();

    let mut index = 0usize;
    while index < args.len() {
        let argument = &args[index];
        match argument.as_str() {
            "--strict" => strict = true,
            "--deep" => deep = true,
            "--fast" => deep = false,
            "--backend" => {
                let value = next_value(args, &mut index, argument)?;
                backend = Backend::parse(&value)?;
            }
            other if other.starts_with("--") => positional.push(other.to_string()),
            other => positional.push(other.to_string()),
        }
        index += 1;
    }

    if positional.is_empty() {
        return Err(AppError::usage("verify requires <file.edz>"));
    }

    let mut failures = 0usize;
    for file in positional {
        let path = absolute(&file);
        let result = verify::run(&path, strict, deep, backend);
        println!("EDZ     : {}", path.display());
        println!("  OK    : {}", result.ok);
        println!(
            "  Pkgs  : {}   Items: {}   Missing: {}   Manifest v{}",
            result.packages,
            result.items,
            result.missing,
            result.manifest_version.as_deref().unwrap_or("")
        );
        for warning in result.warnings.iter().take(10) {
            println!("  warn  : {warning}");
        }
        for error in result.errors.iter().take(20) {
            println!("  error : {error}");
        }
        if !result.ok {
            failures += 1;
        }
    }

    Ok(if failures == 0 { 0 } else { 4 })
}

// ---------------------------------------------------------------- case-a

fn cmd_case_a(args: &[String]) -> Result<i32> {
    let mut out_dir: Option<String> = None;
    let mut backend = Backend::Auto;
    let mut positional: Vec<String> = Vec::new();

    let mut index = 0usize;
    while index < args.len() {
        let argument = &args[index];
        match argument.as_str() {
            "--out" => out_dir = Some(next_value(args, &mut index, argument)?),
            "--verify" => {}
            "--backend" => {
                let value = next_value(args, &mut index, argument)?;
                backend = Backend::parse(&value)?;
            }
            other if other.starts_with("--") => {
                return Err(AppError::usage(format!("Unknown option: {other}")));
            }
            other => positional.push(other.to_string()),
        }
        index += 1;
    }

    if positional.is_empty() {
        return Err(AppError::usage("case-a requires <archive>"));
    }
    let out_dir = out_dir.ok_or_else(|| AppError::usage("--out <dir> is required"))?;

    commands::case_a(&commands::CaseAOptions {
        archive: absolute(&positional[0]),
        out_dir: absolute(&out_dir),
        backend,
    })
}

// ---------------------------------------------------------------- usage

pub fn print_usage() {
    println!(
        r#"edzsplit - split a multi-device EPLAN EDZ into one EDZ per part.

USAGE
  edzsplit inspect <archive> [--json] [--backend auto|native|cli]
      Show container, entries, manifest, package/item counts, reference repair stats.

  edzsplit split <archive> --out <dir> [options]
      Options:
        --only <p1,p2>          Split only the listed part numbers.
        --from-file <file>      Part numbers to split, one per line ('#' comments).
        --closure <mode>        none | related | full   (default: full)
        --fix-refs <mode>       auto | never            (default: auto)
        --verify-structural     Verify each output EDZ (default)
        --verify-deep           Also decode every output entry while verifying
        --no-verify             Skip per-output structural verification
        --resume                Skip parts already recorded as done in <out>/.edzsplit-state.jsonl
        --keep-cache            Keep the intermediate extraction cache (debugging)
        --keep-going            Continue after individual part failures (default)
        --fail-fast             Stop at the first failed part
        --max-closure-ratio <r> Warn when a closure exceeds r * source bytes (0..1)
        --dry-run               Plan only; write nothing
        --log <path>            Also append JSONL events to <path>
        --workdir <dir>         Override the intermediate work/cache directory
        --backend <mode>        auto | native | cli      (default: auto)
        --jobs <n|auto>         Parallel part writers (default: auto)
        --compress <mode>       copy | lzma2[:level]    (default: copy)
        --strict-part           Keep exactly one 'part' package per output
        --progress              Print progress lines to stderr

  edzsplit verify <file.edz>... [--strict] [--fast] [--backend auto|native|cli]
      Structural verification: 7z integrity, manifest graph, reference resolution, traversal guard.
      --fast skips the full container decode.

  edzsplit case-a <archive> --out <dir>
      Case A: archive already contains nested .edz files - extract and verify them.

  edzsplit selftest
      Build a synthetic two-part EDZ, split it, verify the outputs (no EPLAN needed).

EXIT CODES
  0 ok - 2 usage error - 3 finished with warnings/quarantine - 4 failed

Each split run prints JSONL events to stdout; pipe to a file for logs/evidence."#
    );
}
