use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use wait_timeout::ChildExt;

use crate::archive::{ArchiveSource, Member, ensure_member_path_safe};
use crate::error::{AppError, Result};
use crate::util::{normalize_member_path, safe_disk_path, short_id, tail};

pub const ENV_SEVEN_ZIP: &str = "EDZSPLIT_7Z";

struct ProcessOutput {
    code: i32,
    stdout: String,
    stderr: String,
}

pub fn find_seven_zip() -> Result<PathBuf> {
    if let Ok(from_env) = std::env::var(ENV_SEVEN_ZIP) {
        let candidate = PathBuf::from(from_env);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    if let Some(path_env) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_env) {
            for name in ["7z", "7z.exe", "7za", "7zr"] {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
    }

    for location in [
        r"C:\Program Files\7-Zip\7z.exe",
        r"C:\Program Files (x86)\7-Zip\7z.exe",
        "/usr/bin/7z",
        "/usr/local/bin/7z",
    ] {
        let candidate = PathBuf::from(location);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    Err(AppError::message(
        "7-Zip (7z) was not found. Install p7zip on Linux (e.g. 'sudo apt install p7zip-full') \
         or 7-Zip on Windows, or point EDZSPLIT_7Z at the 7z executable.",
    ))
}

fn run(
    seven_zip: &Path,
    arguments: &[OsString],
    working_dir: Option<&Path>,
    timeout: Duration,
) -> Result<ProcessOutput> {
    let mut command = Command::new(seven_zip);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = working_dir {
        command.current_dir(dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = command
        .spawn()
        .map_err(|error| AppError::message(format!("failed to start 7z: {error}")))?;

    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let stdout_thread = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(pipe) = stdout_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buffer);
        }
        buffer
    });
    let stderr_thread = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(pipe) = stderr_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buffer);
        }
        buffer
    });

    let status = match child
        .wait_timeout(timeout)
        .map_err(|error| AppError::message(format!("failed to wait for 7z: {error}")))?
    {
        Some(status) => status,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(AppError::message(format!(
                "7z timed out after {} ms",
                timeout.as_millis()
            )));
        }
    };

    let stdout = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();
    Ok(ProcessOutput {
        code: status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

pub struct CliSevenZ {
    seven_zip: PathBuf,
    archive: PathBuf,
    members: Vec<Member>,
    cache_dir: PathBuf,
    extract_all_done: AtomicBool,
    prepare_lock: Mutex<()>,
    ephemeral_lock: Mutex<()>,
}

impl CliSevenZ {
    pub fn open(path: &Path, cache_dir: &Path) -> Result<Self> {
        let seven_zip = find_seven_zip()?;
        let archive = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let members = list(&seven_zip, &archive)?;
        Ok(Self {
            seven_zip,
            archive,
            members,
            cache_dir: cache_dir.to_path_buf(),
            extract_all_done: AtomicBool::new(false),
            prepare_lock: Mutex::new(()),
            ephemeral_lock: Mutex::new(()),
        })
    }

    fn extract_members_to(&self, members: &[String], dest: &Path) -> Result<()> {
        for member in members {
            ensure_member_path_safe(member)?;
        }
        std::fs::create_dir_all(dest)?;
        std::fs::create_dir_all(&self.cache_dir)?;
        let list_file = self
            .cache_dir
            .join(format!(".extract-list-{}.txt", short_id()));
        let mut content = String::new();
        for member in members {
            content.push_str(&normalize_member_path(member).replace('\\', "/"));
            content.push('\n');
        }
        std::fs::write(&list_file, content.as_bytes())?;

        let arguments = vec![
            OsString::from("x"),
            OsString::from("-y"),
            OsString::from("-spd"),
            OsString::from("-scsUTF-8"),
            OsString::from("-sccUTF-8"),
            OsString::from("-bso0"),
            OsString::from("-bse0"),
            OsString::from("-bsp0"),
            self.archive.clone().into_os_string(),
            OsString::from(format!("@{}", list_file.display())),
            OsString::from(format!("-o{}", dest.display())),
        ];
        let outcome = run(&self.seven_zip, &arguments, None, Duration::from_secs(3600));
        let _ = std::fs::remove_file(&list_file);

        let output = outcome?;
        if output.code != 0 {
            return Err(AppError::message(format!(
                "7z extraction failed (exit {}): {}",
                output.code,
                tail(&format!("{}{}", output.stdout, output.stderr), 2000)
            )));
        }
        Ok(())
    }

    fn extract_all_to(&self, dest: &Path) -> Result<()> {
        for member in &self.members {
            ensure_member_path_safe(&member.path)?;
        }
        std::fs::create_dir_all(dest)?;
        let arguments = vec![
            OsString::from("x"),
            OsString::from("-y"),
            OsString::from("-spd"),
            OsString::from("-sccUTF-8"),
            OsString::from("-bso0"),
            OsString::from("-bse0"),
            OsString::from("-bsp0"),
            self.archive.clone().into_os_string(),
            OsString::from(format!("-o{}", dest.display())),
        ];
        let output = run(&self.seven_zip, &arguments, None, Duration::from_secs(3600))?;
        if output.code != 0 {
            return Err(AppError::message(format!(
                "7z extraction failed (exit {}): {}",
                output.code,
                tail(&format!("{}{}", output.stdout, output.stderr), 2000)
            )));
        }
        Ok(())
    }

    fn ensure_extracted(&self) -> Result<()> {
        if self.extract_all_done.load(Ordering::Acquire) {
            return Ok(());
        }
        let _guard = self
            .prepare_lock
            .lock()
            .map_err(|_| AppError::message("7z prepare lock poisoned"))?;
        if self.extract_all_done.load(Ordering::Acquire) {
            return Ok(());
        }
        self.extract_all_to(&self.cache_dir)?;
        self.extract_all_done.store(true, Ordering::Release);
        Ok(())
    }

    fn read_ephemeral(&self, member: &Member) -> Result<Vec<u8>> {
        // Keep concurrent single-member extractions from thrashing the same dir.
        let _guard = self
            .ephemeral_lock
            .lock()
            .map_err(|_| AppError::message("7z read lock poisoned"))?;
        let scratch = self.cache_dir.join(format!(".member-{}", short_id()));
        let result = (|| {
            self.extract_members_to(std::slice::from_ref(&member.path), &scratch)?;
            let path = safe_disk_path(&scratch, &member.path)?;
            std::fs::read(&path).map_err(|error| {
                AppError::message(format!(
                    "7z extracted no file for {} ({}): {}",
                    member.path,
                    path.display(),
                    error
                ))
            })
        })();
        let _ = std::fs::remove_dir_all(&scratch);
        result
    }
}

impl ArchiveSource for CliSevenZ {
    fn backend_name(&self) -> &'static str {
        "cli"
    }

    fn members(&self) -> &[Member] {
        &self.members
    }

    fn read(&self, member: &Member) -> Result<Vec<u8>> {
        if self.extract_all_done.load(Ordering::Acquire) {
            let path = safe_disk_path(&self.cache_dir, &member.path)?;
            return std::fs::read(&path).map_err(|error| {
                AppError::message(format!(
                    "cached member missing ({}): {}",
                    path.display(),
                    error
                ))
            });
        }
        self.read_ephemeral(member)
    }

    fn prepare(&self) -> Result<Option<PathBuf>> {
        self.ensure_extracted()?;
        Ok(Some(self.cache_dir.clone()))
    }

    fn extract_all(&self, dest: &Path) -> Result<()> {
        self.extract_all_to(dest)
    }

    fn test(&self) -> Result<()> {
        let arguments = vec![
            OsString::from("t"),
            OsString::from("-sccUTF-8"),
            self.archive.clone().into_os_string(),
        ];
        let output = run(&self.seven_zip, &arguments, None, Duration::from_secs(3600))?;
        if output.code != 0 {
            return Err(AppError::message(format!(
                "7z test failed: {}",
                tail(&format!("{}{}", output.stdout, output.stderr), 1500)
            )));
        }
        Ok(())
    }
}

fn list(seven_zip: &Path, archive: &Path) -> Result<Vec<Member>> {
    let arguments = vec![
        OsString::from("l"),
        OsString::from("-slt"),
        OsString::from("-sccUTF-8"),
        archive.as_os_str().to_os_string(),
    ];
    let output = run(seven_zip, &arguments, None, Duration::from_secs(600))?;
    if output.code != 0 {
        return Err(AppError::message(format!(
            "7z list failed (exit {}) for {}: {}",
            output.code,
            archive.display(),
            tail(&output.stderr, 2000)
        )));
    }

    let mut members = Vec::new();
    for block in split_blocks(&output.stdout) {
        let Some(path) = block.get("path") else {
            continue;
        };
        if path.is_empty() || same_file(path, archive) {
            continue;
        }

        let size = block
            .get("size")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        let is_dir = block.get("folder").is_some_and(|value| value == "+")
            || block
                .get("attributes")
                .is_some_and(|value| value.split_whitespace().any(|part| part == "D"));

        members.push(Member::new(path, size, is_dir));
    }
    Ok(members)
}

fn split_blocks(stdout: &str) -> Vec<HashMap<String, String>> {
    let mut blocks = Vec::new();
    let mut current: HashMap<String, String> = HashMap::new();
    for line in stdout.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                blocks.push(std::mem::take(&mut current));
            }
            continue;
        }
        if let Some(index) = line.find('=') {
            if index == 0 {
                continue;
            }
            let key = line[..index].trim().to_lowercase();
            let value = line[index + 1..].trim().to_string();
            if !key.is_empty() {
                current.insert(key, value);
            }
        }
    }
    if !current.is_empty() {
        blocks.push(current);
    }
    blocks
}

fn same_file(candidate: &str, archive: &Path) -> bool {
    let candidate = Path::new(candidate);
    let left = std::fs::canonicalize(candidate).unwrap_or_else(|_| candidate.to_path_buf());
    let right = std::fs::canonicalize(archive).unwrap_or_else(|_| archive.to_path_buf());
    if cfg!(windows) {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    } else {
        left == right
    }
}
