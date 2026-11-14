use std::path::Path;

use crate::archive::{ArchiveIndex, Backend, open_archive};
use crate::error::Result;
use crate::model::Manifest;
use crate::util::{base_name, create_temp_dir, is_unsafe_member_path, tail};

#[derive(Debug, Default)]
pub struct VerifyResult {
    pub ok: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub packages: usize,
    pub items: usize,
    pub missing: usize,
    pub manifest_version: Option<String>,
}

pub fn run(edz: &Path, strict: bool, deep: bool, backend: Backend) -> VerifyResult {
    let mut result = VerifyResult::default();
    let path = match std::path::absolute(edz) {
        Ok(path) => path,
        Err(_) => edz.to_path_buf(),
    };
    if !path.is_file() {
        result
            .errors
            .push(format!("File not found: {}", path.display()));
        return result;
    }
    if !crate::util::is_7z_file(&path) && !crate::util::is_zip_file(&path) {
        result
            .errors
            .push(format!("Not a 7z or ZIP container: {}", path.display()));
        return result;
    }

    let cache_dir = match create_temp_dir("edzsplit-verify") {
        Ok(dir) => dir,
        Err(error) => {
            result.errors.push(error.to_string());
            return result;
        }
    };
    let outcome = verify_with_source(&path, strict, deep, backend, &cache_dir, &mut result);
    let _ = std::fs::remove_dir_all(&cache_dir);

    if let Err(error) = outcome {
        result.errors.push(error.to_string());
    }

    result.ok = result.errors.is_empty() && (!strict || result.warnings.is_empty());
    result
}

fn verify_with_source(
    path: &Path,
    _strict: bool,
    deep: bool,
    backend: Backend,
    cache_dir: &Path,
    result: &mut VerifyResult,
) -> Result<()> {
    let source = open_archive(path, backend, cache_dir, 4)?;

    if deep {
        source.test()?;
    }

    let members = source.members().to_vec();
    let index = ArchiveIndex::new(members.clone());

    for member in &members {
        if member.path.is_empty() {
            continue;
        }
        if is_unsafe_member_path(&member.path) {
            result
                .errors
                .push(format!("Unsafe entry path: {}", member.path));
        }
    }
    if !result.errors.is_empty() {
        return Ok(());
    }

    let direct: Vec<usize> = members
        .iter()
        .enumerate()
        .filter(|(_, member)| member.path.eq_ignore_ascii_case("manifest.xml"))
        .map(|(index, _)| index)
        .collect();
    let candidates: Vec<usize> = if direct.is_empty() {
        members
            .iter()
            .enumerate()
            .filter(|(_, member)| base_name(&member.path).eq_ignore_ascii_case("manifest.xml"))
            .map(|(index, _)| index)
            .collect()
    } else {
        direct
    };

    if candidates.is_empty() {
        result.errors.push("manifest.xml missing.".to_string());
        return Ok(());
    }

    let manifest_bytes = source.read(&members[candidates[0]])?;
    let model = match Manifest::parse_bytes(&manifest_bytes) {
        Ok(model) => model,
        Err(error) => {
            result
                .errors
                .push(format!("manifest.xml parse failed: {error}"));
            return Ok(());
        }
    };

    result.packages = model.packages.len();
    result.items = model.all_items().count();
    result.manifest_version = Some(model.version.clone());

    for item in model.all_items() {
        match index.try_resolve(&item.member_path()) {
            Some((_, repaired)) => {
                if repaired {
                    result
                        .warnings
                        .push(format!("Repaired reference: {}", item.member_path()));
                }
            }
            None => {
                result.missing += 1;
                result.errors.push(format!(
                    "Missing item: {}/{} -> {}",
                    item.item_type,
                    item.name,
                    item.member_path()
                ));
            }
        }
    }

    let known_types = [
        "part",
        "manufacturer",
        "construction",
        "connectionpoints",
        "accessorylist",
        "accessoryplacement",
    ];
    for package in &model.packages {
        if !known_types
            .iter()
            .any(|known| package.package_type.eq_ignore_ascii_case(known))
        {
            result
                .warnings
                .push(format!("Unknown package type: {}", package.package_type));
        }
    }

    Ok(())
}

pub fn summarize_errors(result: &VerifyResult, limit: usize) -> String {
    tail(
        &result
            .errors
            .iter()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>()
            .join("; "),
        1500,
    )
}
