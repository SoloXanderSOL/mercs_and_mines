use std::path::Path;
use sha2::{Sha256, Digest};
use tokio::io::AsyncReadExt;

pub struct SessionIntegrity {
    pub session_id:    String,
    pub seed:          u64,
    pub build_version: String,
    pub log_hash:      String,
}

/// Reads the completed session log, parses the SessionConfig header for
/// metadata, and computes a SHA-256 digest of the full file contents.
/// The file is read once into memory — log files are bounded in size
/// (one NDJSON line per tick; a 50-tick session is a few KB).
pub async fn compute_session_integrity(
    log_dir:    &Path,
    session_id: &str,
) -> std::io::Result<SessionIntegrity> {
    let path = log_dir.join(format!("{}.ndjson", session_id));
    let mut file = tokio::fs::File::open(&path).await?;

    let mut buf = Vec::new();
    file.read_to_end(&mut buf).await?;

    // Parse SessionConfig from the first line — contains seed + build_version.
    let first_line = buf
        .split(|&b| b == b'\n')
        .next()
        .ok_or_else(|| io_err("log file is empty"))?;
    let config: shared::SessionConfig = serde_json::from_slice(first_line)
        .map_err(io_data_err)?;

    // Hash the full file (header + all InputLogEntry lines).
    let mut hasher = Sha256::new();
    hasher.update(&buf);
    let log_hash = hasher
        .finalize()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>();

    Ok(SessionIntegrity {
        session_id:    config.session_id,
        seed:          config.seed,
        build_version: config.build_version,
        log_hash,
    })
}

fn io_err(msg: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg)
}

fn io_data_err(e: impl std::error::Error + Send + Sync + 'static) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, e)
}
