pub mod cli7z;
pub mod native;
pub mod writer;
pub mod zip_source;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::{AppError, Result};
use crate::util::{is_unsafe_member_path, normalize_member_path, sanitize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Auto,
    Native,
    Cli,
}

impl Backend {
    pub fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "native" => Ok(Self::Native),
            "cli" | "7z" => Ok(Self::Cli),
            other => Err(AppError::usage(format!("Unknown backend: {other}"))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerKind {
    SevenZ,
    Zip,
}

#[derive(Debug, Clone)]
pub struct Member {
    /// Archive name normalized to backslashes, without a leading separator.
    pub path: String,
    /// Name exactly as stored in the container (used by archive readers).
    pub exact_name: String,
    pub size: u64,
    pub is_dir: bool,
}

impl Member {
    pub fn new(exact_name: &str, size: u64, is_dir: bool) -> Self {
        Self {
            path: normalize_member_path(exact_name),
            exact_name: exact_name.to_string(),
            size,
            is_dir,
        }
    }
}

/// Common interface for the native 7z reader, the 7z CLI and the ZIP reader.
pub trait ArchiveSource: Send + Sync {
    fn backend_name(&self) -> &'static str;
    fn members(&self) -> &[Member];

    fn read(&self, member: &Member) -> Result<Vec<u8>>;

    /// Materialize the cache used by backends that cannot read random members cheaply.
    fn prepare(&self) -> Result<Option<PathBuf>> {
        Ok(None)
    }

    fn extract_all(&self, dest: &Path) -> Result<()>;
    fn test(&self) -> Result<()>;
}

pub fn detect_container(path: &Path) -> Result<ContainerKind> {
    if crate::util::is_7z_file(path) {
        return Ok(ContainerKind::SevenZ);
    }
    if crate::util::is_zip_file(path) {
        return Ok(ContainerKind::Zip);
    }
    Err(AppError::message(format!(
        "Input is neither 7z nor ZIP: {}",
        path.display()
    )))
}

/// Opens an archive with the requested backend, falling back to the 7z CLI in auto mode.
pub fn open_archive(
    path: &Path,
    backend: Backend,
    cache_dir: &Path,
    jobs: usize,
) -> Result<Box<dyn ArchiveSource>> {
    let container = detect_container(path)?;
    match (backend, container) {
        (Backend::Native, ContainerKind::Zip) => Err(AppError::usage(
            "--backend native only supports 7z containers; use --backend auto for ZIP",
        )),
        (Backend::Native, ContainerKind::SevenZ) => {
            Ok(Box::new(native::NativeSevenZ::open(path, cache_dir, jobs)?))
        }
        (Backend::Cli, _) => Ok(Box::new(cli7z::CliSevenZ::open(path, cache_dir)?)),
        (Backend::Auto, ContainerKind::Zip) => Ok(Box::new(zip_source::ZipSource::open(path)?)),
        (Backend::Auto, ContainerKind::SevenZ) => {
            match native::NativeSevenZ::open(path, cache_dir, jobs) {
                Ok(source) => Ok(Box::new(source)),
                Err(native_error) => match cli7z::CliSevenZ::open(path, cache_dir) {
                    Ok(source) => {
                        eprintln!(
                            "warning: native 7z backend unavailable ({native_error}); using 7z CLI"
                        );
                        Ok(Box::new(source))
                    }
                    Err(cli_error) => Err(AppError::message(format!(
                        "cannot open 7z archive: native backend: {native_error}; 7z CLI: {cli_error}"
                    ))),
                },
            }
        }
    }
}

/// Archive member index with exact, slash-normalized and non-ASCII-sanitized lookups.
pub struct ArchiveIndex {
    members: Vec<Member>,
    by_path: HashMap<String, usize>,
    by_sanitized: HashMap<String, usize>,
}

impl ArchiveIndex {
    pub fn new(members: Vec<Member>) -> Self {
        let mut by_path: HashMap<String, usize> = HashMap::new();
        let mut order: Vec<usize> = Vec::with_capacity(members.len());
        for (index, member) in members.iter().enumerate() {
            let key = lookup_key(&member.path);
            match by_path.entry(key) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(index);
                    order.push(index);
                }
                std::collections::hash_map::Entry::Occupied(_) => {}
            }
        }

        let mut by_sanitized: HashMap<String, usize> = HashMap::new();
        for index in order {
            let key = lookup_key(&sanitize(&members[index].path));
            by_sanitized.entry(key).or_insert(index);
        }

        Self {
            members,
            by_path,
            by_sanitized,
        }
    }

    pub fn member(&self, index: usize) -> &Member {
        &self.members[index]
    }

    /// Returns the member index and whether the match needed non-ASCII sanitization.
    pub fn try_resolve(&self, member_path: &str) -> Option<(usize, bool)> {
        let normalized = normalize_member_path(member_path);
        let key = lookup_key(&normalized);
        if let Some(index) = self.by_path.get(&key) {
            return Some((*index, false));
        }
        let sanitized = lookup_key(&sanitize(&normalized));
        self.by_sanitized
            .get(&sanitized)
            .map(|index| (*index, true))
    }
}

pub fn lookup_key(path: &str) -> String {
    normalize_member_path(path).to_lowercase()
}

pub fn ensure_member_path_safe(member_path: &str) -> Result<()> {
    if is_unsafe_member_path(member_path) {
        return Err(AppError::message(format!(
            "Unsafe entry path: {member_path}"
        )));
    }
    Ok(())
}
