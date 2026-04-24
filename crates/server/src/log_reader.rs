use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, BufReader};
use shared::{InputLogEntry, SessionConfig};
use crate::api_types::CombatResolveRequest;

pub struct ParsedSession {
    pub config: SessionConfig,
    pub request: CombatResolveRequest,
    /// Original wall-clock timestamp from session creation.
    /// Must be passed to resolve_combat() unchanged — never replaced with now().
    pub timestamp: String,
}

pub async fn load_combat_session(
    log_dir: &Path,
    session_id: &str,
) -> std::io::Result<ParsedSession> {
    let path = log_dir.join(format!("{}.ndjson", session_id));
    let file = File::open(&path).await?;
    let mut lines = BufReader::new(file).lines();

    // Line 1: SessionConfig header
    let header = lines.next_line().await?
        .ok_or_else(|| io_err("log file is empty"))?;
    let config: SessionConfig = serde_json::from_str(&header)
        .map_err(io_data_err)?;

    // Find the session_start InputLogEntry (line 2 by convention; scan to be safe)
    while let Some(line) = lines.next_line().await? {
        let entry: InputLogEntry = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(_) => continue,
        };
        if entry.event_type != "session_start" {
            continue;
        }
        let request: CombatResolveRequest =
            serde_json::from_value(entry.payload["request"].clone())
                .map_err(io_data_err)?;
        let timestamp = entry.payload["timestamp"]
            .as_str()
            .ok_or_else(|| io_err("session_start payload missing timestamp field"))?
            .to_string();
        return Ok(ParsedSession { config, request, timestamp });
    }

    Err(io_err("no session_start entry found"))
}

fn io_err(msg: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg)
}

fn io_data_err(e: impl std::error::Error + Send + Sync + 'static) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, e)
}
