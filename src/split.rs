use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rayon::prelude::*;
use serde_json::{Map, Value, json};

use crate::archive::writer::{self, CopyEntry, EntrySource};
use crate::archive::{ArchiveIndex, ArchiveSource, Backend, Member, lookup_key, open_archive};
use crate::error::{AppError, Result};
use crate::event::EventLog;
use crate::graph::{ArchivePayloads, ClosureMode, DependencyGraph, package_node};
use crate::model::{Manifest, Package};
use crate::payload;
use crate::util::{decode_text, normalize_member_path, sanitize_file_name, sha256_file, short_id};
use crate::verify;

const STATE_FILE_NAME: &str = ".edzsplit-state.jsonl";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    Copy,
    Lzma2(u32),
}

impl Compression {
    pub fn parse(value: &str) -> Result<Self> {
        let lower = value.to_ascii_lowercase();
        if lower == "copy" {
            return Ok(Self::Copy);
        }
        if lower == "lzma2" {
            return Ok(Self::Lzma2(6));
        }
        if let Some(level) = lower.strip_prefix("lzma2:") {
            let level: u32 = level
                .parse()
                .map_err(|_| AppError::usage("--compress level must be 0..9"))?;
            if level > 9 {
                return Err(AppError::usage("--compress level must be 0..9"));
            }
            return Ok(Self::Lzma2(level));
        }
        Err(AppError::usage(
            "--compress must be copy, lzma2 or lzma2:<0..9>",
        ))
    }
}

#[derive(Debug, Clone)]
pub struct SplitOptions {
    pub archive: PathBuf,
    pub out_dir: PathBuf,
    pub only: Vec<String>,
    pub only_from_file: Option<PathBuf>,
    pub closure: ClosureMode,
    pub fix_refs: String,
    pub verify_structural: bool,
    pub verify_deep: bool,
    pub resume: bool,
    pub keep_cache: bool,
    pub keep_going: bool,
    pub max_closure_ratio: f64,
    pub dry_run: bool,
    pub work_dir: Option<PathBuf>,
    pub log_path: Option<PathBuf>,
    pub backend: Backend,
    pub jobs: usize,
    pub compress: Compression,
    pub strict_part: bool,
    pub progress: bool,
}

impl SplitOptions {
    pub fn new(archive: PathBuf, out_dir: PathBuf) -> Self {
        Self {
            archive,
            out_dir,
            only: Vec::new(),
            only_from_file: None,
            closure: ClosureMode::Full,
            fix_refs: "auto".to_string(),
            verify_structural: true,
            verify_deep: false,
            resume: false,
            keep_cache: false,
            keep_going: true,
            max_closure_ratio: 1.0,
            dry_run: false,
            work_dir: None,
            log_path: None,
            backend: Backend::Auto,
            jobs: default_jobs(),
            compress: Compression::Copy,
            strict_part: false,
            progress: false,
        }
    }
}

pub fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(4)
        .min(8)
}

#[derive(Debug, Default)]
pub struct SplitReport {
    pub parts_planned: usize,
    pub parts_written: usize,
    pub parts_skipped_resume: usize,
    pub parts_failed: usize,
    pub total_bytes_written: u64,
    pub failed: Vec<String>,
    pub warnings: Vec<String>,
}

impl SplitReport {
    pub fn exit_code(&self) -> i32 {
        if self.parts_failed == 0 {
            if self.warnings.is_empty() { 0 } else { 3 }
        } else if self.parts_written > 0 {
            3
        } else {
            4
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PartStatus {
    Planned,
    Written,
    Skipped,
    Failed,
    Cancelled,
}

struct PartOutcome {
    part: String,
    status: PartStatus,
    bytes: u64,
    warnings: Vec<String>,
    error: Option<String>,
}

pub fn run(options: &SplitOptions) -> Result<SplitReport> {
    if !options.archive.is_file() {
        return Err(AppError::message(format!(
            "Input archive not found: {}",
            options.archive.display()
        )));
    }
    let archive = std::path::absolute(&options.archive).unwrap_or_else(|_| options.archive.clone());
    crate::archive::detect_container(&archive)
        .map_err(|_| AppError::message("Input is neither 7z nor ZIP."))?;

    let out_dir = std::path::absolute(&options.out_dir).unwrap_or_else(|_| options.out_dir.clone());
    std::fs::create_dir_all(&out_dir)?;

    let run_id = crate::util::run_id();
    let (work_dir, auto_work_dir) = match &options.work_dir {
        Some(dir) => (dir.clone(), false),
        None => (out_dir.join(format!(".work-{run_id}")), true),
    };
    std::fs::create_dir_all(&work_dir)?;
    let cache_dir = work_dir.join("cache");
    std::fs::create_dir_all(&cache_dir)?;

    let result = run_inner(options, &archive, &out_dir, &work_dir, &cache_dir);

    if !options.keep_cache {
        if auto_work_dir {
            let _ = std::fs::remove_dir_all(&work_dir);
        } else {
            let _ = std::fs::remove_dir_all(&cache_dir);
        }
    }

    result
}

fn run_inner(
    options: &SplitOptions,
    archive: &Path,
    out_dir: &Path,
    _work_dir: &Path,
    cache_dir: &Path,
) -> Result<SplitReport> {
    let events = EventLog::new(options.log_path.as_deref())?;
    let mut report = SplitReport::default();

    events.write(
        "split.begin",
        json!({
            "archive": archive.to_string_lossy(),
            "outDir": out_dir.to_string_lossy(),
            "closure": options.closure.as_str(),
            "fixRefs": options.fix_refs,
            "dryRun": options.dry_run
        }),
    );

    let source: Arc<dyn ArchiveSource> = Arc::from(open_archive(
        archive,
        options.backend,
        cache_dir,
        options.jobs,
    )?);
    events.write(
        "backend.selected",
        json!({ "backend": source.backend_name() }),
    );

    let members: Vec<Member> = source.members().to_vec();
    let index = ArchiveIndex::new(members.clone());
    events.write(
        "archive.listed",
        json!({
            "entries": members.len(),
            "totalBytes": members.iter().map(|member| member.size).sum::<u64>()
        }),
    );

    let manifest_text = read_manifest(&*source, &index, &events)?;
    let model = match Manifest::parse(&manifest_text) {
        Ok(model) => model,
        Err(error) => {
            events.write("manifest.error", json!({ "error": error.to_string() }));
            return Err(error);
        }
    };

    let mut package_types: Map<String, Value> = Map::new();
    for (name, count) in count_in_order(
        model
            .packages
            .iter()
            .map(|package| package.package_type.as_str()),
    ) {
        package_types.insert(name, json!(count));
    }
    let mut item_names: Map<String, Value> = Map::new();
    for (name, count) in count_in_order(model.all_items().map(|item| item.name.as_str())) {
        item_names.insert(name, json!(count));
    }
    events.write(
        "manifest.parsed",
        json!({
            "packages": model.packages.len(),
            "items": model.all_items().count(),
            "packageTypes": package_types,
            "itemNameKinds": item_names
        }),
    );

    let part_packages: Vec<usize> = model
        .packages
        .iter()
        .enumerate()
        .filter(|(_, package)| package.package_type.eq_ignore_ascii_case("part"))
        .map(|(index, _)| index)
        .collect();

    let (selected, not_found) = select_parts(&model, &part_packages, options)?;
    for partnr in &not_found {
        events.write("part.not-found", json!({ "partnr": partnr }));
    }
    if !not_found.is_empty() {
        report.warnings.push(format!(
            "Parts not found in archive: {}",
            not_found.join(", ")
        ));
    }

    let selected = dedupe_selected(&model, selected, &events, &mut report.warnings);
    report.parts_planned = selected.len();

    if !options.dry_run
        && let Some(cache) = source.prepare()?
    {
        events.write(
            "cache.extracted",
            json!({ "cacheDir": cache.to_string_lossy() }),
        );
    }

    let payloads = ArchivePayloads::new(&*source, &index);
    let graph = DependencyGraph::build(&model, &index, &payloads);

    let state_path = out_dir.join(STATE_FILE_NAME);
    let done = if options.resume {
        load_done(&state_path)
    } else {
        HashSet::new()
    };
    events.write(
        "resume.state",
        json!({ "resume": options.resume, "alreadyDone": done.len() }),
    );

    let cancel = AtomicBool::new(false);
    let progress_count = AtomicUsize::new(0);
    let state_lock = Mutex::new(());
    let source_bytes = std::fs::metadata(archive)
        .map(|meta| meta.len())
        .unwrap_or(0);
    let total = selected.len();

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(options.jobs.max(1))
        .build()
        .map_err(|error| AppError::message(format!("cannot create thread pool: {error}")))?;

    let outcomes: Vec<PartOutcome> = pool.install(|| {
        selected
            .par_iter()
            .map(|part_index| {
                if cancel.load(Ordering::Acquire) {
                    return PartOutcome {
                        part: model.packages[*part_index].key.clone(),
                        status: PartStatus::Cancelled,
                        bytes: 0,
                        warnings: Vec::new(),
                        error: None,
                    };
                }
                split_one(
                    options,
                    &model,
                    &index,
                    &*source,
                    &graph,
                    &payloads,
                    &events,
                    &done,
                    &state_path,
                    &state_lock,
                    &cancel,
                    &progress_count,
                    total,
                    source_bytes,
                    *part_index,
                )
            })
            .collect()
    });

    for outcome in outcomes {
        match outcome.status {
            PartStatus::Planned => {}
            PartStatus::Written => {
                report.parts_written += 1;
                report.total_bytes_written += outcome.bytes;
            }
            PartStatus::Skipped => {
                report.parts_skipped_resume += 1;
            }
            PartStatus::Cancelled => {}
            PartStatus::Failed => {
                report.parts_failed += 1;
                report.failed.push(format!(
                    "{}: {}",
                    outcome.part,
                    outcome.error.unwrap_or_default()
                ));
            }
        }
        report.warnings.extend(outcome.warnings);
    }

    if !options.keep_going && report.parts_failed > 0 {
        let first = report
            .failed
            .first()
            .cloned()
            .unwrap_or_else(|| "first failure".to_string());
        return Err(AppError::message(format!("stopped after failure: {first}")));
    }

    events.write(
        "split.summary",
        json!({
            "planned": report.parts_planned,
            "written": report.parts_written,
            "resumed": report.parts_skipped_resume,
            "failed": report.parts_failed,
            "totalBytes": report.total_bytes_written,
            "warnings": report.warnings.len()
        }),
    );

    for warning in &report.warnings {
        events.write("warning", json!({ "message": warning }));
    }

    Ok(report)
}

#[allow(clippy::too_many_arguments)]
fn split_one(
    options: &SplitOptions,
    model: &Manifest,
    index: &ArchiveIndex,
    source: &dyn ArchiveSource,
    graph: &DependencyGraph<'_>,
    payloads: &ArchivePayloads<'_>,
    events: &EventLog,
    done: &HashSet<String>,
    state_path: &Path,
    state_lock: &Mutex<()>,
    cancel: &AtomicBool,
    progress_count: &AtomicUsize,
    total: usize,
    source_bytes: u64,
    part_index: usize,
) -> PartOutcome {
    let key = model.packages[part_index].key.clone();
    let edz_name = format!("{}.edz", sanitize_file_name(&key));
    let edz_path = options.out_dir.join(&edz_name);

    if options.resume && done.contains(&key) && edz_path.is_file() {
        return PartOutcome {
            part: key,
            status: PartStatus::Skipped,
            bytes: 0,
            warnings: Vec::new(),
            error: None,
        };
    }

    let finish = |status: PartStatus, bytes: u64, warnings: Vec<String>, error: Option<String>| {
        let done_count = progress_count.fetch_add(1, Ordering::AcqRel) + 1;
        if options.progress {
            eprintln!("[{done_count}/{total}] {key}");
        }
        PartOutcome {
            part: key.clone(),
            status,
            bytes,
            warnings,
            error,
        }
    };

    let result = (|| -> Result<PartOutcomeInner> {
        let closure =
            graph.compute_closure(part_index, options.closure, options.strict_part, payloads);

        let mut seen_items: HashSet<String> = HashSet::new();
        let mut items: Vec<(usize, usize)> = Vec::new();
        for (package_index, item_index) in &closure.items {
            let member_path = model.packages[*package_index].items[*item_index].member_path();
            if seen_items.insert(lookup_key(&member_path)) {
                items.push((*package_index, *item_index));
            }
        }

        let mut seen_packages: HashSet<String> = HashSet::new();
        let mut packages: Vec<usize> = Vec::new();
        for package_index in &closure.packages {
            if seen_packages.insert(package_node(&model.packages[*package_index])) {
                packages.push(*package_index);
            }
        }

        let mut warnings: Vec<String> = Vec::new();
        let mut estimated_bytes: u64 = 0;
        for (package_index, item_index) in &items {
            let item = &model.packages[*package_index].items[*item_index];
            if let Some((member_index, _)) = index.try_resolve(&item.member_path()) {
                estimated_bytes += index.member(member_index).size;
            }
        }

        if options.max_closure_ratio < 1.0
            && source_bytes > 0
            && estimated_bytes as f64 > source_bytes as f64 * options.max_closure_ratio
        {
            warnings.push(format!(
                "Closure for {key} is {} bytes (> {:.0}% of source)",
                format_thousands(estimated_bytes),
                options.max_closure_ratio * 100.0
            ));
        }

        if options.dry_run {
            let package_names: Vec<String> = packages
                .iter()
                .map(|package_index| {
                    let package = &model.packages[*package_index];
                    format!("{}:{}", package.package_type, package.key)
                })
                .collect();
            events.write(
                "part.plan",
                json!({
                    "part": key,
                    "packages": package_names,
                    "items": items.len(),
                    "estimatedBytes": estimated_bytes
                }),
            );
            return Ok(PartOutcomeInner {
                status: PartStatus::Planned,
                warnings,
                bytes: 0,
            });
        }

        if cancel.load(Ordering::Acquire) {
            return Ok(PartOutcomeInner {
                status: PartStatus::Cancelled,
                warnings,
                bytes: 0,
            });
        }

        let fix_refs_auto = options.fix_refs.eq_ignore_ascii_case("auto");
        let mut payload_cache: HashMap<String, Arc<Vec<u8>>> = HashMap::new();
        if fix_refs_auto {
            for (package_index, item_index) in &items {
                let item = &model.packages[*package_index].items[*item_index];
                if !item.item_type.eq_ignore_ascii_case("partxml") {
                    continue;
                }
                let Some((member_index, _)) = index.try_resolve(&item.member_path()) else {
                    continue;
                };
                let member = index.member(member_index);
                let bytes = source.read(member)?;
                let text = decode_text(&bytes);
                let (rewritten, rewrites) = payload::rewrite_payload_refs(&text, index);
                let member_key = lookup_key(&member.path);
                if rewrites > 0 {
                    events.write(
                        "payload.rewritten",
                        json!({
                            "part": key,
                            "item": item.member_path(),
                            "rewrites": rewrites
                        }),
                    );
                    payload_cache.insert(member_key, Arc::new(rewritten.into_bytes()));
                } else {
                    // Keep the original bytes so the writer does not decode twice.
                    payload_cache.insert(member_key, Arc::new(bytes));
                }
            }
        }

        let mut manifest_clone = Manifest {
            version: model.version.clone(),
            packages: Vec::new(),
        };
        for package_index in &packages {
            let package = &model.packages[*package_index];
            let mut clone = Package {
                package_type: package.package_type.clone(),
                key: package.key.clone(),
                name: package.name.clone(),
                items: Vec::new(),
            };
            let mut dropped = 0usize;
            for source_item in &package.items {
                let member_path = source_item.member_path();
                match index.try_resolve(&member_path) {
                    Some((member_index, _)) => {
                        let mut item = source_item.clone();
                        let normalized_resolved =
                            normalize_member_path(&index.member(member_index).path);
                        if !normalized_resolved.eq_ignore_ascii_case(&member_path) {
                            let prefix = format!("items\\{}\\", item.item_type);
                            let prefix_matches = normalized_resolved.len() >= prefix.len()
                                && normalized_resolved[..prefix.len()]
                                    .eq_ignore_ascii_case(&prefix);
                            item.locator = if prefix_matches {
                                normalized_resolved[prefix.len()..].to_string()
                            } else {
                                normalized_resolved
                            };
                            events.write(
                                "locator.repaired",
                                json!({
                                    "part": key,
                                    "from": source_item.locator,
                                    "to": item.locator
                                }),
                            );
                        }
                        clone.items.push(item);
                    }
                    None => {
                        dropped += 1;
                    }
                }
            }
            if dropped > 0 {
                events.write("item.dropped", json!({ "part": key, "count": dropped }));
            }
            manifest_clone.packages.push(clone);
        }

        for package_index in &packages {
            for missing in &model.packages[*package_index].items {
                if graph.is_missing(missing) {
                    warnings.push(format!(
                        "Missing item for {key}: {}/{}",
                        missing.item_type, missing.locator
                    ));
                    events.write(
                        "item.missing",
                        json!({
                            "part": key,
                            "type": missing.item_type,
                            "name": missing.name,
                            "locator": missing.locator
                        }),
                    );
                }
            }
        }

        let mut entries: Vec<CopyEntry> = Vec::new();
        let mut seen_entries: HashSet<String> = HashSet::new();
        for clone_package in &manifest_clone.packages {
            for clone_item in &clone_package.items {
                let member_path = normalize_member_path(&clone_item.member_path());
                if !seen_entries.insert(lookup_key(&member_path)) {
                    continue;
                }
                if let Some(bytes) = payload_cache.get(&lookup_key(&member_path)) {
                    entries.push(CopyEntry {
                        key: member_path,
                        size: bytes.len() as u64,
                        source: EntrySource::Bytes(bytes.clone()),
                    });
                    continue;
                }
                let Some((member_index, _)) = index.try_resolve(&member_path) else {
                    continue;
                };
                let member = index.member(member_index);
                if member.is_dir {
                    continue;
                }
                entries.push(CopyEntry {
                    key: member_path,
                    size: member.size,
                    source: EntrySource::Member(member_index),
                });
            }
        }

        let manifest_bytes = Arc::new(manifest_clone.to_xml()?.into_bytes());
        entries.push(CopyEntry {
            key: "manifest.xml".to_string(),
            size: manifest_bytes.len() as u64,
            source: EntrySource::Bytes(manifest_bytes),
        });

        let tmp_path = PathBuf::from(format!("{}.tmp-{}", edz_path.to_string_lossy(), short_id()));
        let read_member = |member_index: usize| source.read(index.member(member_index));

        let write_result = match options.compress {
            Compression::Copy => writer::write_copy(&tmp_path, &entries, &read_member),
            Compression::Lzma2(level) => {
                writer::write_compressed(&tmp_path, &entries, &read_member, level)
            }
        };

        if let Err(error) = write_result {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(error);
        }

        if options.verify_structural {
            let verified = verify::run(&tmp_path, false, options.verify_deep, Backend::Auto);
            if !verified.ok {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(AppError::message(format!(
                    "output verification failed: {}",
                    verify::summarize_errors(&verified, 10)
                )));
            }
        }

        let sha256 = match sha256_file(&tmp_path) {
            Ok(sha256) => sha256,
            Err(error) => {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(error);
            }
        };
        if let Err(error) = std::fs::rename(&tmp_path, &edz_path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(AppError::from(error));
        }

        append_state(
            state_path,
            json!({
                "partnr": key,
                "status": "done",
                "file": edz_path.to_string_lossy(),
                "sha256": sha256,
                "items": entries.len()
            }),
            state_lock,
        )?;

        let bytes = std::fs::metadata(&edz_path)
            .map(|meta| meta.len())
            .unwrap_or(0);
        events.write(
            "part.written",
            json!({
                "part": key,
                "file": edz_path.to_string_lossy(),
                "sha256": sha256,
                "entries": entries.len()
            }),
        );

        Ok(PartOutcomeInner {
            status: PartStatus::Written,
            warnings,
            bytes,
        })
    })();

    match result {
        Ok(inner) => finish(inner.status, inner.bytes, inner.warnings, None),
        Err(error) => {
            if !options.keep_going {
                cancel.store(true, Ordering::Release);
            }
            events.write(
                "part.failed",
                json!({ "part": key, "error": error.to_string() }),
            );
            let _ = append_state(
                state_path,
                json!({
                    "partnr": key,
                    "status": "failed",
                    "error": error.to_string()
                }),
                state_lock,
            );
            finish(PartStatus::Failed, 0, Vec::new(), Some(error.to_string()))
        }
    }
}

struct PartOutcomeInner {
    status: PartStatus,
    warnings: Vec<String>,
    bytes: u64,
}

fn select_parts(
    model: &Manifest,
    part_packages: &[usize],
    options: &SplitOptions,
) -> Result<(Vec<usize>, Vec<String>)> {
    let mut only = options.only.clone();
    if let Some(path) = &options.only_from_file
        && path.is_file()
    {
        let text = std::fs::read_to_string(path)?;
        for line in text.lines() {
            let line = line.trim();
            if !line.is_empty() && !line.starts_with('#') {
                only.push(line.to_string());
            }
        }
    }

    if only.is_empty() {
        return Ok((part_packages.to_vec(), Vec::new()));
    }

    let mut wanted: Vec<String> = Vec::new();
    let mut wanted_keys: HashSet<String> = HashSet::new();
    for value in only {
        if wanted_keys.insert(value.to_lowercase()) {
            wanted.push(value);
        }
    }

    let mut selected: Vec<usize> = Vec::new();
    let mut selected_keys: HashSet<String> = HashSet::new();
    for package_index in part_packages {
        let package = &model.packages[*package_index];
        if wanted_keys.contains(&package.key.to_lowercase()) {
            selected.push(*package_index);
            selected_keys.insert(package.key.to_lowercase());
        }
    }

    let missing = wanted
        .into_iter()
        .filter(|value| !selected_keys.contains(&value.to_lowercase()))
        .collect();

    Ok((selected, missing))
}

fn dedupe_selected(
    model: &Manifest,
    selected: Vec<usize>,
    events: &EventLog,
    warnings: &mut Vec<String>,
) -> Vec<usize> {
    let mut result: Vec<usize> = Vec::with_capacity(selected.len());
    let mut positions: HashMap<String, usize> = HashMap::new();

    for package_index in selected {
        let package = &model.packages[package_index];
        let key = package.key.to_lowercase();
        if let Some(position) = positions.get(&key).copied() {
            let previous = &model.packages[result[position]];
            if same_items(previous, package) {
                events.write(
                    "part.duplicate",
                    json!({ "partnr": package.key, "kind": "identical" }),
                );
                continue;
            }
            events.write(
                "part.duplicate",
                json!({ "partnr": package.key, "kind": "conflict" }),
            );
            warnings.push(format!(
                "Duplicate part {} has differing item sets; the last package wins",
                package.key
            ));
            result[position] = package_index;
            continue;
        }
        positions.insert(key, result.len());
        result.push(package_index);
    }

    result
}

fn same_items(left: &Package, right: &Package) -> bool {
    left.items.len() == right.items.len()
        && left
            .items
            .iter()
            .zip(right.items.iter())
            .all(|(left, right)| {
                left.item_type.eq_ignore_ascii_case(&right.item_type)
                    && left.name.eq_ignore_ascii_case(&right.name)
                    && left.locator.eq_ignore_ascii_case(&right.locator)
            })
}

fn read_manifest(
    source: &dyn ArchiveSource,
    index: &ArchiveIndex,
    events: &EventLog,
) -> Result<String> {
    if let Some((member_index, _)) = index.try_resolve("manifest.xml") {
        let bytes = source.read(index.member(member_index))?;
        return Ok(decode_text(&bytes));
    }

    let candidates: Vec<&Member> = source
        .members()
        .iter()
        .filter(|member| crate::util::base_name(&member.path).eq_ignore_ascii_case("manifest.xml"))
        .collect();

    if let Some(member) = candidates.first() {
        events.write("manifest.found", json!({ "entry": member.path }));
        let bytes = source.read(member)?;
        return Ok(decode_text(&bytes));
    }

    Err(AppError::message(
        "manifest.xml not found in archive (neither at root nor under a folder).",
    ))
}

fn load_done(state_path: &Path) -> HashSet<String> {
    let mut done = HashSet::new();
    let Ok(text) = std::fs::read_to_string(state_path) else {
        return done;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let part = value.get("partnr").and_then(Value::as_str);
        let status = value.get("status").and_then(Value::as_str);
        if let (Some(part), Some(status)) = (part, status)
            && status.eq_ignore_ascii_case("done")
        {
            done.insert(part.to_string());
        }
    }
    done
}

fn append_state(state_path: &Path, record: Value, lock: &Mutex<()>) -> Result<()> {
    let _guard = lock
        .lock()
        .map_err(|_| AppError::message("state lock poisoned"))?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state_path)?;
    file.write_all(record.to_string().as_bytes())?;
    file.write_all(b"\n")?;
    file.flush()?;
    let _ = file.sync_data();
    Ok(())
}

fn count_in_order<'a>(values: impl Iterator<Item = &'a str>) -> Vec<(String, usize)> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    let mut positions: HashMap<String, usize> = HashMap::new();
    for value in values {
        if let Some(position) = positions.get(value) {
            counts[*position].1 += 1;
        } else {
            positions.insert(value.to_string(), counts.len());
            counts.push((value.to_string(), 1));
        }
    }
    counts
}

fn format_thousands(value: u64) -> String {
    let text = value.to_string();
    let mut out = String::with_capacity(text.len() + text.len() / 3);
    for (index, ch) in text.chars().enumerate() {
        if index > 0 && (text.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub fn split_summary(report: &SplitReport) -> String {
    let mut out = String::new();
    out.push_str("\n=== Split summary ===\n");
    out.push_str(&format!("Planned : {}\n", report.parts_planned));
    out.push_str(&format!(
        "Written : {}  ({} bytes)\n",
        report.parts_written, report.total_bytes_written
    ));
    out.push_str(&format!(
        "Resume  : {} skipped\n",
        report.parts_skipped_resume
    ));
    out.push_str(&format!("Failed  : {}\n", report.parts_failed));
    for failure in report.failed.iter().take(20) {
        out.push_str(&format!("  failed: {failure}\n"));
    }
    if !report.warnings.is_empty() {
        out.push_str(&format!(
            "Warnings: {} (see log for details)\n",
            report.warnings.len()
        ));
    }
    out
}
