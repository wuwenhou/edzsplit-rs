use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use crate::archive::{ArchiveIndex, Backend, open_archive};
use crate::error::{AppError, Result};
use crate::model::Manifest;
use crate::util::{base_name, create_temp_dir, safe_disk_path};
use crate::verify;

pub struct InspectOptions {
    pub archive: PathBuf,
    pub json: bool,
    pub backend: Backend,
}

pub fn inspect(options: &InspectOptions) -> Result<i32> {
    if !options.archive.is_file() {
        return Err(AppError::usage(format!(
            "Archive not found: {}",
            options.archive.display()
        )));
    }
    let archive = std::path::absolute(&options.archive).unwrap_or_else(|_| options.archive.clone());
    let cache_dir = create_temp_dir("edzsplit-inspect")?;

    let outcome = (|| -> Result<i32> {
        let source = open_archive(&archive, options.backend, &cache_dir, 4)?;
        let members = source.members().to_vec();
        let index = ArchiveIndex::new(members.clone());

        let container = if crate::util::is_7z_file(&archive) {
            "7z"
        } else {
            "zip"
        };
        let file_size = std::fs::metadata(&archive)?.len();
        let total_uncompressed: u64 = members.iter().map(|member| member.size).sum();
        let nested_edz = members
            .iter()
            .filter(|member| member.path.to_lowercase().ends_with(".edz"))
            .count();

        let manifest = read_manifest(&*source, &members, &index);

        let mut stats = Map::new();
        stats.insert("archive".to_string(), json!(archive.to_string_lossy()));
        stats.insert("container".to_string(), json!(container));
        stats.insert("fileSizeBytes".to_string(), json!(file_size));
        stats.insert("entries".to_string(), json!(members.len()));
        stats.insert(
            "totalUncompressedBytes".to_string(),
            json!(total_uncompressed),
        );
        stats.insert("nestedEdzEntries".to_string(), json!(nested_edz));
        stats.insert("hasManifest".to_string(), json!(manifest.is_some()));
        stats.insert(
            "manifestVersion".to_string(),
            json!(manifest.as_ref().map(|model| model.version.clone())),
        );
        stats.insert(
            "packages".to_string(),
            json!(manifest.as_ref().map(|model| model.packages.len())),
        );
        stats.insert(
            "items".to_string(),
            json!(manifest.as_ref().map(|model| model.all_items().count())),
        );

        let mut package_types: Option<Vec<(String, usize)>> = None;
        let mut direct = 0usize;
        let mut repaired = 0usize;
        let mut missing = 0usize;
        let mut missing_unique: Vec<String> = Vec::new();
        let mut missing_seen: HashSet<String> = HashSet::new();

        if let Some(model) = &manifest {
            package_types = Some(count_in_order(
                model
                    .packages
                    .iter()
                    .map(|package| package.package_type.as_str()),
            ));
            let item_names = count_in_order(model.all_items().map(|item| item.name.as_str()));

            let mut types = Map::new();
            for (name, count) in package_types.as_ref().unwrap() {
                types.insert(name.clone(), json!(count));
            }
            stats.insert("packageTypes".to_string(), Value::Object(types));

            let mut names = Map::new();
            for (name, count) in &item_names {
                names.insert(name.clone(), json!(count));
            }
            stats.insert("itemNames".to_string(), Value::Object(names));

            for item in model.all_items() {
                match index.try_resolve(&item.member_path()) {
                    Some((_, was_repaired)) => {
                        if was_repaired {
                            repaired += 1;
                        } else {
                            direct += 1;
                        }
                    }
                    None => {
                        missing += 1;
                        if missing_seen.insert(item.member_path()) {
                            missing_unique.push(item.member_path());
                        }
                    }
                }
            }

            stats.insert(
                "refs".to_string(),
                json!({
                    "direct": direct,
                    "repaired": repaired,
                    "missing": missing,
                    "missingUnique": missing_unique.len()
                }),
            );
            stats.insert(
                "missingUniqueSamples".to_string(),
                json!(missing_unique.iter().take(20).collect::<Vec<_>>()),
            );
            stats.insert(
                "partPackages".to_string(),
                json!(
                    model
                        .packages
                        .iter()
                        .filter(|package| package.package_type.eq_ignore_ascii_case("part"))
                        .count()
                ),
            );
        }

        if options.json {
            println!("{}", serde_json::to_string_pretty(&Value::Object(stats))?);
            return Ok(0);
        }

        println!("Archive   : {}", archive.display());
        println!("Container : {container}  ({file_size} bytes)");
        println!(
            "Entries   : {}   (uncompressed {total_uncompressed} bytes)",
            members.len()
        );
        println!("Nested EDZ: {nested_edz}");

        let Some(model) = &manifest else {
            println!("Manifest  : MISSING or unreadable");
            return Ok(4);
        };
        println!("Manifest  : v{}", model.version);
        let types = package_types
            .as_ref()
            .map(|types| {
                types
                    .iter()
                    .map(|(name, count)| format!("{name}={count}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        println!("Packages  : {}  ({types})", model.packages.len());
        println!("Items     : {}", model.all_items().count());
        println!(
            "Refs      : direct={direct} repaired={repaired} missing={missing} (unique missing={})",
            missing_unique.len()
        );
        if nested_edz > 0 && model.packages.is_empty() {
            println!(
                "Hint      : archive looks like Case A (EDZ files inside an archive) - use `edzsplit case-a`."
            );
        }
        Ok(0)
    })();

    let _ = std::fs::remove_dir_all(&cache_dir);
    outcome
}

fn read_manifest(
    source: &dyn crate::archive::ArchiveSource,
    members: &[crate::archive::Member],
    index: &ArchiveIndex,
) -> Option<Manifest> {
    if let Some((member_index, _)) = index.try_resolve("manifest.xml")
        && let Ok(bytes) = source.read(index.member(member_index))
        && let Ok(model) = Manifest::parse_bytes(&bytes)
    {
        return Some(model);
    }

    for (member_index, member) in members.iter().enumerate() {
        if base_name(&member.path).eq_ignore_ascii_case("manifest.xml")
            && let Ok(bytes) = source.read(index.member(member_index))
            && let Ok(model) = Manifest::parse_bytes(&bytes)
        {
            return Some(model);
        }
    }
    None
}

pub struct CaseAOptions {
    pub archive: PathBuf,
    pub out_dir: PathBuf,
    pub backend: Backend,
}

pub fn case_a(options: &CaseAOptions) -> Result<i32> {
    if !options.archive.is_file() {
        return Err(AppError::usage(format!(
            "Archive not found: {}",
            options.archive.display()
        )));
    }
    let archive = std::path::absolute(&options.archive).unwrap_or_else(|_| options.archive.clone());
    std::fs::create_dir_all(&options.out_dir)?;

    let cache_dir = create_temp_dir("edzsplit-case-a")?;

    let outcome = (|| -> Result<i32> {
        let source = open_archive(&archive, options.backend, &cache_dir, 4)?;
        let nested: Vec<_> = source
            .members()
            .iter()
            .filter(|member| member.path.to_lowercase().ends_with(".edz"))
            .cloned()
            .collect();
        if nested.is_empty() {
            eprintln!("No nested .edz entries found - not a Case A archive.");
            return Ok(2);
        }

        println!("Case A archive: {}", archive.display());
        println!("Nested EDZ     : {}", nested.len());

        source.extract_all(&options.out_dir)?;

        let mut ok_count = 0usize;
        let mut bad_count = 0usize;
        for member in &nested {
            let path = safe_disk_path(&options.out_dir, &member.path)?;
            if !path.is_file() {
                println!("MISSING: {}", member.path);
                bad_count += 1;
                continue;
            }
            let result = verify::run(&path, false, false, Backend::Auto);
            if result.ok {
                ok_count += 1;
                println!("OK    : {}", member.path);
            } else {
                bad_count += 1;
                println!(
                    "BAD   : {} ({})",
                    member.path,
                    verify::summarize_errors(&result, 3)
                );
            }
        }

        println!(
            "Extracted {} EDZ files to {}: ok={}, bad={}",
            nested.len(),
            options.out_dir.display(),
            ok_count,
            bad_count
        );
        Ok(if bad_count == 0 { 0 } else { 3 })
    })();

    let _ = std::fs::remove_dir_all(&cache_dir);
    outcome
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
