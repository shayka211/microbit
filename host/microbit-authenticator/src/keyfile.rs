//! The host's copy of the shared secret (64 hex chars + newline).

use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use authenticate::protocol::Key;

pub const DEFAULT_PATH: &str = "/etc/microbit-auth/secret.key";

/// `MICROBIT_KEY_FILE`, or [`DEFAULT_PATH`].
pub fn path() -> PathBuf {
    std::env::var_os("MICROBIT_KEY_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_PATH))
}

pub fn parse_hex(s: &str) -> Result<Key> {
    let bytes = hex::decode(s.trim()).map_err(|_| anyhow!("key is not valid hex"))?;
    bytes
        .try_into()
        .map_err(|_| anyhow!("key must be exactly 32 bytes (64 hex chars)"))
}

/// Load the key. Returns a warning text if the file is readable by others.
pub fn load(path: &Path) -> Result<(Key, Option<String>)> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read key file {}", path.display()))?;
    let key = parse_hex(&raw).with_context(|| format!("bad key file {}", path.display()))?;
    let mode = std::fs::metadata(path)?.mode() & 0o777;
    let warning = (mode & 0o077 != 0).then(|| {
        format!(
            "key file {} has mode {mode:o}; it should be 600",
            path.display()
        )
    });
    Ok((key, warning))
}

/// Write `key` to a new file created with mode 0600 (never briefly world-readable).
pub fn save(path: &Path, key: &Key) -> Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("cannot write {}", path.display()))?;
    f.write_all(format!("{}\n", hex::encode(key)).as_bytes())?;
    f.sync_all()?;
    Ok(())
}
