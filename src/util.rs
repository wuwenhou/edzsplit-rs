use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::error::{AppError, Result};

pub fn normalize_slashes(value: &str) -> String {
    value.replace('/', "\\")
}

pub fn normalize_member_path(value: &str) -> String {
    normalize_slashes(value)
        .trim_start_matches('\\')
        .to_string()
}

pub fn item_member_path(item_type: &str, locator: &str) -> String {
    format!("items\\{}\\{}", item_type, normalize_member_path(locator))
}

/// Mirrors `ArchiveIndex.Sanitize`: every UTF-16 code unit >= 128 becomes '_'.
pub fn sanitize(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for unit in value.encode_utf16() {
        if unit < 128 {
            out.push(unit as u8 as char);
        } else {
            out.push('_');
        }
    }
    out
}

/// Mirrors `Path.GetInvalidFileNameChars()` on Windows, applied on every platform.
pub fn sanitize_file_name(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if (ch as u32) < 32
                || matches!(ch, '"' | '<' | '>' | '|' | ':' | '*' | '?' | '\\' | '/')
            {
                '_'
            } else {
                ch
            }
        })
        .collect()
}

pub fn decode_text(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(&bytes[3..]).into_owned();
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub fn now_iso() -> String {
    let now = chrono::Utc::now();
    format!(
        "{}.{:07}+00:00",
        now.format("%Y-%m-%dT%H:%M:%S"),
        now.timestamp_subsec_nanos() / 100
    )
}

pub fn utc_stamp() -> String {
    chrono::Utc::now().format("%Y%m%d-%H%M%S-%3f").to_string()
}

pub fn run_id() -> String {
    format!("{}-{}", utc_stamp(), short_id())
}

pub fn short_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id() as u64;
    let mut h = 0x9E37_79B9_7F4A_7C15u64 ^ nanos ^ (pid << 32) ^ n;
    h = (h ^ (h >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h = (h ^ (h >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^= h >> 31;
    format!("{h:016x}")[..8].to_string()
}

pub fn tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let start = text.len() - max;
    let start = crate::util::nearest_char_boundary(text, start);
    text[start..].to_string()
}

pub fn nearest_char_boundary(text: &str, mut index: usize) -> usize {
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

pub fn is_7z_file(path: &Path) -> bool {
    let mut header = [0u8; 6];
    match File::open(path).and_then(|mut f| f.read_exact(&mut header)) {
        Ok(()) => header == [0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C],
        Err(_) => false,
    }
}

pub fn is_zip_file(path: &Path) -> bool {
    let mut header = [0u8; 2];
    match File::open(path).and_then(|mut f| f.read_exact(&mut header)) {
        Ok(()) => header == [0x50, 0x4B],
        Err(_) => false,
    }
}

pub fn safe_disk_path(root: &Path, member_path: &str) -> Result<PathBuf> {
    if is_unsafe_member_path(member_path) {
        return Err(AppError::message(format!(
            "Unsafe entry path: {member_path}"
        )));
    }
    let mut path = root.to_path_buf();
    for segment in normalize_slashes(member_path).split('\\') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." || segment.contains(':') {
            return Err(AppError::message(format!(
                "Unsafe entry path: {member_path}"
            )));
        }
        path.push(segment);
    }
    Ok(path)
}

pub fn is_unsafe_member_path(member_path: &str) -> bool {
    let normalized = normalize_slashes(member_path);
    if normalized.trim().is_empty() {
        return true;
    }
    if normalized.starts_with('\\') {
        return true;
    }
    if normalized.len() >= 2 && normalized.as_bytes()[1] == b':' {
        return true;
    }
    normalized
        .split('\\')
        .any(|segment| segment == ".." || segment.contains(':'))
}

/// Unique temporary directory inside a writable root.
///
/// Some hardened machines deny writes to the system temp directory; falling back
/// to the current directory keeps `selftest` and `verify` usable there.
pub fn create_temp_dir(prefix: &str) -> Result<PathBuf> {
    let root = temp_root();
    let dir = root.join(format!("{prefix}-{}", short_id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn temp_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("EDZSPLIT_TMP") {
        return PathBuf::from(dir);
    }
    let candidate = std::env::temp_dir();
    if is_writable_dir(&candidate) {
        return candidate;
    }
    if let Ok(cwd) = std::env::current_dir()
        && is_writable_dir(&cwd)
    {
        return cwd;
    }
    candidate
}

fn is_writable_dir(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let probe = dir.join(format!(".edzsplit-probe-{}", short_id()));
    match std::fs::create_dir_all(&probe) {
        Ok(()) => {
            let _ = std::fs::remove_dir(&probe);
            true
        }
        Err(_) => false,
    }
}

pub fn base_name(member_path: &str) -> String {
    normalize_member_path(member_path)
        .rsplit('\\')
        .next()
        .unwrap_or("")
        .to_string()
}

pub fn file_stem(file_name: &str) -> String {
    match file_name.rfind('.') {
        Some(index) if index > 0 => file_name[..index].to_string(),
        _ => file_name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_slashes() {
        assert_eq!(
            normalize_member_path("items/macro/x.ema"),
            "items\\macro\\x.ema"
        );
        assert_eq!(
            normalize_member_path("/items//macro/x.ema"),
            "items\\\\macro\\x.ema"
        );
    }

    #[test]
    fn sanitizes_like_dotnet() {
        assert_eq!(sanitize("Schutzgeräte"), "Schutzger_te");
        assert_eq!(sanitize("abc/def"), "abc/def");
    }

    #[test]
    fn sanitizes_file_names() {
        assert_eq!(sanitize_file_name("A:B*C?D"), "A_B_C_D");
    }

    #[test]
    fn rejects_traversal() {
        assert!(is_unsafe_member_path("..\\evil"));
        assert!(is_unsafe_member_path("items\\..\\evil"));
        assert!(is_unsafe_member_path("\\absolute"));
        assert!(is_unsafe_member_path("C:\\absolute"));
        assert!(!is_unsafe_member_path("items\\partxml\\a.xml"));
    }

    #[test]
    fn file_stem_matches_dotnet() {
        assert_eq!(file_stem("ABC.connectionpoints"), "ABC");
        assert_eq!(
            file_stem("ABC.connectionpoints.xml"),
            "ABC.connectionpoints"
        );
        assert_eq!(file_stem("noext"), "noext");
    }
}
