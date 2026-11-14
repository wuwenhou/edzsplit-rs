use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sevenz_rust2::{ArchiveReader, Password};

use crate::archive::{ArchiveSource, Member, ensure_member_path_safe};
use crate::error::{AppError, Result};
use crate::util::{normalize_member_path, safe_disk_path};

/// Native 7z backend. Non-solid archives are read lazily one member at a time;
/// solid archives are extracted once into the cache directory.
pub struct NativeSevenZ {
    path: PathBuf,
    members: Vec<Member>,
    by_exact_name: HashMap<String, usize>,
    pool: Mutex<Vec<ArchiveReader<File>>>,
    max_readers: usize,
    solid: bool,
    cache_dir: PathBuf,
    cache_state: Mutex<Option<std::result::Result<(), String>>>,
}

impl NativeSevenZ {
    pub fn open(path: &Path, cache_dir: &Path, jobs: usize) -> Result<Self> {
        let reader = ArchiveReader::open(path, Password::empty())?;
        let archive = reader.archive();
        let solid = archive.is_solid;

        let mut members = Vec::with_capacity(archive.files.len());
        let mut by_exact_name = HashMap::new();
        for entry in &archive.files {
            if entry.name.is_empty() || entry.is_anti_item {
                continue;
            }
            let member = Member::new(&entry.name, entry.size, entry.is_directory);
            if by_exact_name
                .insert(entry.name.clone(), members.len())
                .is_none()
            {
                members.push(member);
            }
        }

        Ok(Self {
            path: path.to_path_buf(),
            members,
            by_exact_name,
            pool: Mutex::new(vec![reader]),
            max_readers: jobs.clamp(1, 32),
            solid,
            cache_dir: cache_dir.to_path_buf(),
            cache_state: Mutex::new(None),
        })
    }

    fn new_reader(&self) -> Result<ArchiveReader<File>> {
        ArchiveReader::open(&self.path, Password::empty()).map_err(AppError::from)
    }

    fn with_reader<T>(
        &self,
        operation: impl FnOnce(&mut ArchiveReader<File>) -> Result<T>,
    ) -> Result<T> {
        let existing = {
            let mut pool = self
                .pool
                .lock()
                .map_err(|_| AppError::message("native reader pool poisoned"))?;
            pool.pop()
        };
        let mut reader = match existing {
            Some(reader) => reader,
            None => self.new_reader()?,
        };
        let outcome = operation(&mut reader);
        if let Ok(mut pool) = self.pool.lock()
            && pool.len() < self.max_readers
        {
            pool.push(reader);
        }
        outcome
    }

    fn ensure_cache(&self) -> Result<()> {
        let mut state = self
            .cache_state
            .lock()
            .map_err(|_| AppError::message("native cache lock poisoned"))?;
        if let Some(result) = state.as_ref() {
            return result.clone().map_err(AppError::message);
        }
        let outcome = self.extract_all_to(&self.cache_dir);
        let stored = match &outcome {
            Ok(()) => Ok(()),
            Err(error) => Err(error.to_string()),
        };
        *state = Some(stored);
        outcome
    }

    fn read_from_cache(&self, member: &Member) -> Result<Vec<u8>> {
        let path = safe_disk_path(&self.cache_dir, &member.path)?;
        std::fs::read(&path).map_err(|error| {
            AppError::message(format!(
                "cached member missing ({}): {}",
                path.display(),
                error
            ))
        })
    }

    fn extract_all_to(&self, dest: &Path) -> Result<()> {
        std::fs::create_dir_all(dest)?;
        let mut reader = self.new_reader()?;
        let mut failure: Option<AppError> = None;

        reader
            .for_each_entries(|entry, data| {
                let name = normalize_member_path(&entry.name);
                if name.is_empty() {
                    return Ok(true);
                }
                if let Err(error) = ensure_member_path_safe(&name) {
                    failure = Some(error);
                    return Ok(false);
                }
                let target = match safe_disk_path(dest, &name) {
                    Ok(target) => target,
                    Err(error) => {
                        failure = Some(error);
                        return Ok(false);
                    }
                };
                if entry.is_directory {
                    if let Err(error) = std::fs::create_dir_all(&target) {
                        failure = Some(AppError::from(error));
                        return Ok(false);
                    }
                    return Ok(true);
                }
                if let Some(parent) = target.parent()
                    && let Err(error) = std::fs::create_dir_all(parent)
                {
                    failure = Some(AppError::from(error));
                    return Ok(false);
                }
                let mut file = match File::create(&target) {
                    Ok(file) => file,
                    Err(error) => {
                        failure = Some(AppError::from(error));
                        return Ok(false);
                    }
                };
                let mut buffer = vec![0u8; 128 * 1024];
                loop {
                    match data.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(read) => {
                            if let Err(error) = file.write_all(&buffer[..read]) {
                                failure = Some(AppError::from(error));
                                return Ok(false);
                            }
                        }
                        Err(error) => {
                            failure = Some(AppError::from(error));
                            return Ok(false);
                        }
                    }
                }
                Ok(true)
            })
            .map_err(AppError::from)?;

        if let Some(error) = failure {
            return Err(error);
        }
        Ok(())
    }
}

impl ArchiveSource for NativeSevenZ {
    fn backend_name(&self) -> &'static str {
        "native"
    }

    fn members(&self) -> &[Member] {
        &self.members
    }

    fn read(&self, member: &Member) -> Result<Vec<u8>> {
        if self.solid {
            self.ensure_cache()?;
            return self.read_from_cache(member);
        }

        if !self.by_exact_name.contains_key(&member.exact_name) {
            return Err(AppError::message(format!(
                "archive member not found: {}",
                member.exact_name
            )));
        }

        let exact_name = member.exact_name.clone();
        let data =
            self.with_reader(|reader| reader.read_file(&exact_name).map_err(AppError::from))?;

        if data.len() as u64 != member.size {
            return Err(AppError::message(format!(
                "member size mismatch for {}: expected {}, decoded {}",
                member.path,
                member.size,
                data.len()
            )));
        }
        Ok(data)
    }

    fn prepare(&self) -> Result<Option<PathBuf>> {
        if self.solid {
            self.ensure_cache()?;
            Ok(Some(self.cache_dir.clone()))
        } else {
            Ok(None)
        }
    }

    fn extract_all(&self, dest: &Path) -> Result<()> {
        self.extract_all_to(dest)
    }

    fn test(&self) -> Result<()> {
        let mut reader = self.new_reader()?;
        let mut sink = std::io::sink();
        reader
            .for_each_entries(|_, data| {
                std::io::copy(data, &mut sink)
                    .map_err(|error| sevenz_rust2::Error::Io(error, "test".into()))?;
                Ok(true)
            })
            .map_err(AppError::from)?;
        Ok(())
    }
}
