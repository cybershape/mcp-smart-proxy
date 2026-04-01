use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::Utc;

#[derive(Clone)]
pub(crate) struct DaemonLogger {
    path: PathBuf,
    file: Arc<Mutex<File>>,
}

impl DaemonLogger {
    pub(crate) fn open(path: PathBuf) -> Result<Self, Box<dyn std::error::Error>> {
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            path,
            file: Arc::new(Mutex::new(file)),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn info(&self, event: &str, message: impl AsRef<str>) {
        self.write_line("INFO", event, message.as_ref());
    }

    pub(crate) fn error(&self, event: &str, message: impl AsRef<str>) {
        self.write_line("ERROR", event, message.as_ref());
    }

    fn write_line(&self, level: &str, event: &str, message: &str) {
        let timestamp = Utc::now().to_rfc3339();
        let sanitized = message.replace('\n', "\\n");
        if let Ok(mut file) = self.file.lock() {
            let _ = writeln!(
                file,
                "{timestamp} level={level} event={event} message={sanitized}"
            );
            let _ = file.flush();
        }
    }
}
