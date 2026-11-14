use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;

use crate::error::{AppError, Result};

const K_HEADER: u8 = 0x01;
const K_MAIN_STREAMS_INFO: u8 = 0x04;
const K_FILES_INFO: u8 = 0x05;
const K_PACK_INFO: u8 = 0x06;
const K_UNPACK_INFO: u8 = 0x07;
const K_SUBSTREAMS_INFO: u8 = 0x08;
const K_SIZE: u8 = 0x09;
const K_CRC: u8 = 0x0A;
const K_FOLDER: u8 = 0x0B;
const K_CODERS_UNPACK_SIZE: u8 = 0x0C;
const K_NUM_UNPACK_STREAM: u8 = 0x0D;
const K_NAME: u8 = 0x11;
const K_END: u8 = 0x00;
const CODER_COPY: u8 = 0x00;

#[derive(Clone)]
pub enum EntrySource {
    Member(usize),
    Bytes(Arc<Vec<u8>>),
}

pub struct CopyEntry {
    pub key: String,
    pub size: u64,
    pub source: EntrySource,
}

/// Reads a member payload by its index in the archive member list.
pub trait MemberReader {
    fn read_member(&self, index: usize) -> Result<Vec<u8>>;
}

impl<F> MemberReader for F
where
    F: Fn(usize) -> Result<Vec<u8>>,
{
    fn read_member(&self, index: usize) -> Result<Vec<u8>> {
        self(index)
    }
}

/// Exact port of `SevenZipCopyWriter`: one folder, one packed stream, Copy coder,
/// UTF-16LE names with forward slashes, CRC-32 of the concatenated data.
pub fn write_copy(path: &Path, entries: &[CopyEntry], reader: &dyn MemberReader) -> Result<()> {
    if entries.is_empty() {
        return Err(AppError::message("At least one entry is required."));
    }

    let keys: Vec<String> = entries
        .iter()
        .map(|entry| entry.key.replace('\\', "/"))
        .collect();

    // Open once for reading and writing: the start-header patch reads back bytes 12..32.
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    file.write_all(&signature_header())?;

    let mut total_size: u64 = 0;
    let mut hasher = crc32fast::Hasher::new();

    for entry in entries {
        match &entry.source {
            EntrySource::Bytes(bytes) => {
                if bytes.len() as u64 != entry.size {
                    return Err(AppError::message(format!(
                        "entry size mismatch for {}: declared {}, actual {}",
                        entry.key,
                        entry.size,
                        bytes.len()
                    )));
                }
                file.write_all(bytes)?;
                hasher.update(bytes);
                total_size += bytes.len() as u64;
            }
            EntrySource::Member(index) => {
                let data = reader.read_member(*index)?;
                if data.len() as u64 != entry.size {
                    return Err(AppError::message(format!(
                        "entry size mismatch for {}: declared {}, decoded {}",
                        entry.key,
                        entry.size,
                        data.len()
                    )));
                }
                file.write_all(&data)?;
                hasher.update(&data);
                total_size += data.len() as u64;
            }
        }
    }

    let folder_crc = hasher.finalize();
    let sizes: Vec<u64> = entries.iter().map(|entry| entry.size).collect();
    let header = build_header(&keys, &sizes, total_size, folder_crc);

    file.seek(SeekFrom::Start(12))?;
    file.write_all(&total_size.to_le_bytes())?;
    file.write_all(&(header.len() as u64).to_le_bytes())?;
    file.write_all(&crc32fast::hash(&header).to_le_bytes())?;

    let mut start_header = [0u8; 20];
    file.seek(SeekFrom::Start(12))?;
    file.read_exact(&mut start_header)?;
    file.seek(SeekFrom::Start(8))?;
    file.write_all(&crc32fast::hash(&start_header).to_le_bytes())?;

    file.seek(SeekFrom::End(0))?;
    file.write_all(&header)?;
    file.flush()?;

    Ok(())
}

fn signature_header() -> [u8; 32] {
    let mut signature = [0u8; 32];
    signature[0] = 0x37;
    signature[1] = 0x7A;
    signature[2] = 0xBC;
    signature[3] = 0xAF;
    signature[4] = 0x27;
    signature[5] = 0x1C;
    signature[6] = 0x00;
    signature[7] = 0x04;
    signature
}

fn build_header(keys: &[String], sizes: &[u64], total_size: u64, folder_crc: u32) -> Vec<u8> {
    let mut header =
        Vec::with_capacity(64 + keys.iter().map(|key| key.len() * 2 + 2).sum::<usize>());
    header.push(K_HEADER);
    header.push(K_MAIN_STREAMS_INFO);

    header.push(K_PACK_INFO);
    write_number(&mut header, 0);
    write_number(&mut header, 1);
    header.push(K_SIZE);
    write_number(&mut header, total_size);
    header.push(K_END);

    header.push(K_UNPACK_INFO);
    header.push(K_FOLDER);
    write_number(&mut header, 1);
    header.push(0x00);
    write_number(&mut header, 1);
    header.push(0x01);
    header.push(CODER_COPY);
    header.push(K_CODERS_UNPACK_SIZE);
    write_number(&mut header, total_size);
    header.push(K_CRC);
    header.push(0x01);
    header.extend_from_slice(&folder_crc.to_le_bytes());
    header.push(K_END);

    header.push(K_SUBSTREAMS_INFO);
    if keys.len() > 1 {
        header.push(K_NUM_UNPACK_STREAM);
        write_number(&mut header, keys.len() as u64);
        header.push(K_SIZE);
        for size in sizes.iter().take(keys.len() - 1) {
            write_number(&mut header, *size);
        }
    }
    header.push(K_END);
    header.push(K_END);

    header.push(K_FILES_INFO);
    write_number(&mut header, keys.len() as u64);
    header.push(K_NAME);
    let mut name_bytes: u64 = 1;
    for key in keys {
        name_bytes += (key.encode_utf16().count() as u64) * 2 + 2;
    }
    write_number(&mut header, name_bytes);
    header.push(0x00);
    for key in keys {
        for unit in key.encode_utf16() {
            header.extend_from_slice(&unit.to_le_bytes());
        }
        header.push(0x00);
        header.push(0x00);
    }
    header.push(K_END);
    header.push(K_END);

    header
}

/// 7z variable-length number encoding, byte-for-byte identical to the C# writer.
pub fn write_number(out: &mut Vec<u8>, mut value: u64) {
    let mut first_byte: u8 = 0;
    let mut mask: u8 = 0x80;
    let mut index = 0usize;
    while index < 8 {
        if value < (1u64 << (7 * (index + 1))) {
            first_byte |= (value >> (8 * index)) as u8;
            break;
        }
        first_byte |= mask;
        mask >>= 1;
        index += 1;
    }
    out.push(first_byte);
    while index > 0 {
        out.push(value as u8);
        value >>= 8;
        index -= 1;
    }
}

/// Optional LZMA2 output. The crate writes non-solid blocks, which EPLAN accepts,
/// but it is opt-in because Copy is the reference EDZ layout.
pub fn write_compressed(
    path: &Path,
    entries: &[CopyEntry],
    reader: &dyn MemberReader,
    level: u32,
) -> Result<()> {
    use sevenz_rust2::encoder_options::{EncoderOptions, Lzma2Options};
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter, EncoderConfiguration, EncoderMethod};

    let mut writer = ArchiveWriter::create(path)?;
    let method = EncoderConfiguration::new(EncoderMethod::LZMA2)
        .with_options(EncoderOptions::Lzma2(Lzma2Options::from_level(level)));
    writer.set_content_methods(vec![method]);

    for entry in entries {
        let data = match &entry.source {
            EntrySource::Bytes(bytes) => (**bytes).clone(),
            EntrySource::Member(index) => reader.read_member(*index)?,
        };
        if data.len() as u64 != entry.size {
            return Err(AppError::message(format!(
                "entry size mismatch for {}: declared {}, actual {}",
                entry.key,
                entry.size,
                data.len()
            )));
        }
        let mut archive_entry = ArchiveEntry::new();
        archive_entry.name = entry.key.replace('\\', "/");
        let cursor = std::io::Cursor::new(data);
        writer.push_archive_entry(archive_entry, Some(cursor))?;
    }

    writer.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_numbers_like_seven_zip() {
        let cases: &[(u64, &[u8])] = &[
            (0, &[0x00]),
            (1, &[0x01]),
            (0x7F, &[0x7F]),
            (0x80, &[0x80, 0x80]),
            (0x3FFF, &[0xBF, 0xFF]),
            (0x4000, &[0xC0, 0x00, 0x40]),
        ];
        for (value, expected) in cases {
            let mut out = Vec::new();
            write_number(&mut out, *value);
            assert_eq!(&out, expected, "value {value:#x}");
        }
    }

    #[test]
    fn crc32_matches_standard_vector() {
        assert_eq!(crc32fast::hash(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn round_trips_through_native_reader() {
        let dir = crate::util::create_temp_dir("edzsplit-writer-test").unwrap();
        let path = dir.join("test.edz");
        let entries = vec![
            CopyEntry {
                key: "items\\partxml\\A.part.xml".to_string(),
                size: 5,
                source: EntrySource::Bytes(Arc::new(b"hello".to_vec())),
            },
            CopyEntry {
                key: "items\\macro\\ä.ema".to_string(),
                size: 3,
                source: EntrySource::Bytes(Arc::new(b"xyz".to_vec())),
            },
            CopyEntry {
                key: "manifest.xml".to_string(),
                size: 2,
                source: EntrySource::Bytes(Arc::new(b"<m".to_vec())),
            },
        ];
        let reader = |_: usize| -> Result<Vec<u8>> { Err(AppError::message("no members")) };
        write_copy(&path, &entries, &reader).unwrap();

        let mut archive =
            sevenz_rust2::ArchiveReader::open(&path, sevenz_rust2::Password::empty()).unwrap();
        let names: Vec<String> = archive
            .archive()
            .files
            .iter()
            .map(|entry| entry.name.clone())
            .collect();
        assert_eq!(
            names,
            vec![
                "items/partxml/A.part.xml",
                "items/macro/ä.ema",
                "manifest.xml"
            ]
        );
        assert_eq!(archive.read_file("items/macro/ä.ema").unwrap(), b"xyz");
    }
}
