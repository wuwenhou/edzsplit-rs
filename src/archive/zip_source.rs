use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::Mutex;

use zip::ZipArchive;

use crate::archive::{ArchiveSource, Member, ensure_member_path_safe};
use crate::error::{AppError, Result};
use crate::util::safe_disk_path;

pub struct ZipSource {
    archive: Mutex<ZipArchive<File>>,
    members: Vec<Member>,
}

impl ZipSource {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let mut archive = ZipArchive::new(file)?;
        let mut members = Vec::with_capacity(archive.len());
        for index in 0..archive.len() {
            let entry = archive.by_index(index)?;
            let name = entry.name()?.into_owned();
            members.push(Member::new(&name, entry.size(), entry.is_dir()));
        }
        Ok(Self {
            archive: Mutex::new(archive),
            members,
        })
    }
}

impl ArchiveSource for ZipSource {
    fn backend_name(&self) -> &'static str {
        "zip"
    }

    fn members(&self) -> &[Member] {
        &self.members
    }

    fn read(&self, member: &Member) -> Result<Vec<u8>> {
        let mut archive = self
            .archive
            .lock()
            .map_err(|_| AppError::message("ZIP archive lock poisoned"))?;
        let mut entry = archive.by_name(&member.exact_name)?;
        let mut data = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut data)?;
        Ok(data)
    }

    fn extract_all(&self, dest: &Path) -> Result<()> {
        std::fs::create_dir_all(dest)?;
        let mut archive = self
            .archive
            .lock()
            .map_err(|_| AppError::message("ZIP archive lock poisoned"))?;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            let name = entry.name()?.into_owned();
            if name.is_empty() {
                continue;
            }
            ensure_member_path_safe(&name)?;
            let target = safe_disk_path(dest, &name)?;
            if entry.is_dir() {
                std::fs::create_dir_all(&target)?;
                continue;
            }
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut file = File::create(&target)?;
            std::io::copy(&mut entry, &mut file)?;
        }
        Ok(())
    }

    fn test(&self) -> Result<()> {
        let mut archive = self
            .archive
            .lock()
            .map_err(|_| AppError::message("ZIP archive lock poisoned"))?;
        let mut sink = std::io::sink();
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            std::io::copy(&mut entry, &mut sink)?;
        }
        Ok(())
    }
}
