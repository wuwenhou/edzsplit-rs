use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::error::Result;
use crate::util::now_iso;

/// JSONL event sink: one object per line, stdout plus an optional append-only file.
#[derive(Clone, Default)]
pub struct EventLog {
    file: Option<Arc<Mutex<std::fs::File>>>,
}

impl EventLog {
    pub fn new(path: Option<&Path>) -> Result<Self> {
        let file = match path {
            Some(path) if !path.as_os_str().is_empty() => {
                let full = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
                if let Some(parent) = full.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let file = OpenOptions::new().create(true).append(true).open(&full)?;
                Some(Arc::new(Mutex::new(file)))
            }
            _ => None,
        };
        Ok(Self { file })
    }

    pub fn write(&self, event_type: &str, data: Value) {
        let mut record = serde_json::Map::new();
        record.insert("ts".to_string(), Value::String(now_iso()));
        record.insert("type".to_string(), Value::String(event_type.to_string()));
        record.insert("data".to_string(), data);
        let line = Value::Object(record).to_string();
        println!("{line}");
        if let Some(file) = &self.file
            && let Ok(mut file) = file.lock()
        {
            let _ = file.write_all(line.as_bytes());
            let _ = file.write_all(b"\n");
            let _ = file.flush();
        }
    }
}
