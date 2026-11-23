//! Atomic JSON files: write to a temporary file in the same directory, then
//! rename it over the original, so a crash never leaves a half-written file.

use std::io::Write as _;
use std::path::Path;

use anyhow::{Context as _, Result};
use serde::{Serialize, de::DeserializeOwned};

/// Write `value` as pretty JSON to `path`, atomically.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value).context("could not serialize")?;
    write_bytes(path, &bytes)
}

/// Write `bytes` to `path`, atomically.
pub fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    {
        let mut file = std::fs::File::create(&tmp)
            .with_context(|| format!("could not create {}", tmp.display()))?;
        file.write_all(bytes)
            .with_context(|| format!("could not write {}", tmp.display()))?;
        file.sync_all().ok();
    }
    std::fs::rename(&tmp, path).with_context(|| {
        let _ = std::fs::remove_file(&tmp);
        format!("could not replace {}", path.display())
    })?;
    Ok(())
}

/// Read JSON from `path`. A missing file gives `T::default()`. A file that
/// cannot be parsed is kept aside as `<name>.corrupt` and the default is
/// returned, so a bad file never stops the app from starting.
pub fn read_json_or_default<T: DeserializeOwned + Default>(path: &Path) -> T {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(error) => {
                tracing::error!(path = %path.display(), %error, "could not parse; using defaults");
                let aside = path.with_extension("json.corrupt");
                let _ = std::fs::rename(path, aside);
                T::default()
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => T::default(),
        Err(error) => {
            tracing::error!(path = %path.display(), %error, "could not read; using defaults");
            T::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    struct Sample {
        a: u32,
        b: String,
    }

    #[test]
    fn round_trip() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("x/sample.json");
        let value = Sample {
            a: 7,
            b: "hi".into(),
        };
        write_json(&path, &value)?;
        assert_eq!(read_json_or_default::<Sample>(&path), value);
        // No temporary files are left behind.
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap_or(dir.path()))?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
        Ok(())
    }

    #[test]
    fn missing_file_is_default() {
        let value: Sample = read_json_or_default(Path::new("/nonexistent/anveg/x.json"));
        assert_eq!(value, Sample::default());
    }

    #[test]
    fn corrupt_file_is_moved_aside() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("bad.json");
        std::fs::write(&path, b"{not json")?;
        let value: Sample = read_json_or_default(&path);
        assert_eq!(value, Sample::default());
        assert!(!path.exists());
        assert!(dir.path().join("bad.json.corrupt").exists());
        Ok(())
    }

    #[test]
    fn overwrite_replaces() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("s.json");
        write_json(
            &path,
            &Sample {
                a: 1,
                b: "x".into(),
            },
        )?;
        write_json(
            &path,
            &Sample {
                a: 2,
                b: "y".into(),
            },
        )?;
        assert_eq!(read_json_or_default::<Sample>(&path).a, 2);
        Ok(())
    }
}
