use sha2::{Sha256, Digest};
use uuid::Uuid;

use crate::repository::{InputLogRepository, RepositoryError};

pub struct SessionIntegrity {
    pub session_id:    String,
    pub seed:          u64,
    pub build_version: String,
    pub log_hash:      String,
}

/// Reconstructs the session byte sequence from the DB (config header + ordered entries),
/// then computes a SHA-256 digest. Byte-identical to the .ndjson file format written by
/// SessionLogWriter: serde_json::to_string(&record) + "\n" per line.
pub async fn compute_session_integrity(
    repo: &(dyn InputLogRepository + Send + Sync),
    session_id: Uuid,
) -> Result<SessionIntegrity, RepositoryError> {
    let config = repo.get_session_config(&session_id).await?
        .ok_or(RepositoryError::NotFound)?;

    let entries = repo.get_entries_by_session(&session_id).await?;

    let mut buf = Vec::new();
    let header = serde_json::to_string(&config)
        .map_err(|e| RepositoryError::Internal(e.to_string()))?;
    buf.extend_from_slice(header.as_bytes());
    buf.extend_from_slice(b"\n");
    for entry in &entries {
        let line = serde_json::to_string(entry)
            .map_err(|e| RepositoryError::Internal(e.to_string()))?;
        buf.extend_from_slice(line.as_bytes());
        buf.extend_from_slice(b"\n");
    }

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
