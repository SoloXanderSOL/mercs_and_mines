use std::path::{Path, PathBuf};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncWriteExt, BufWriter};
use shared::{InputLogEntry, SessionConfig};

pub struct SessionLogWriter {
    writer: BufWriter<File>,
}

impl SessionLogWriter {
    /// Creates a new log file at <log_dir>/<session_id>.ndjson.
    /// Returns Err if the file already exists (log must be unique per session).
    pub async fn create(log_dir: &Path, session_id: &str) -> std::io::Result<Self> {
        tokio::fs::create_dir_all(log_dir).await?;
        let path: PathBuf = log_dir.join(format!("{}.ndjson", session_id));
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)
            .await?;
        Ok(Self { writer: BufWriter::new(file) })
    }

    /// Writes the SessionConfig as the first line of the log.
    pub async fn write_header(&mut self, config: &SessionConfig) {
        let line = match serde_json::to_string(config) {
            Ok(s) => s,
            Err(e) => { eprintln!("[log_writer] header serialize error: {e}"); return; }
        };
        self.write_line(&line).await;
    }

    /// Appends one InputLogEntry. Infallible from the caller's perspective —
    /// log errors go to stderr and never crash the game session.
    pub async fn append(&mut self, entry: &InputLogEntry) {
        let line = match serde_json::to_string(entry) {
            Ok(s) => s,
            Err(e) => { eprintln!("[log_writer] entry serialize error: {e}"); return; }
        };
        self.write_line(&line).await;
    }

    async fn write_line(&mut self, line: &str) {
        if let Err(e) = async {
            self.writer.write_all(line.as_bytes()).await?;
            self.writer.write_all(b"\n").await?;
            self.writer.flush().await
        }.await {
            eprintln!("[log_writer] write error: {e}");
        }
    }
}
