use std::io;
use std::path::{Path, PathBuf};

/// Errors from loading and saving Meld's configuration files.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not parse {path}: {source}")]
    TomlParse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("could not serialize settings: {0}")]
    TomlSerialize(#[from] toml::ser::Error),
    #[error("invalid style scheme: {0}")]
    InvalidScheme(String),
    #[error("invalid color '{0}'")]
    InvalidColor(String),
    #[error("invalid recent comparison file: {0}")]
    InvalidComparisonFile(String),
    #[error("no configuration directory could be determined")]
    NoConfigDir,
}

impl ConfigError {
    pub(crate) fn io(path: &Path, source: io::Error) -> Self {
        ConfigError::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// Write `contents` to `path` atomically: write a sibling temporary file,
/// sync it and rename it over the destination.
pub(crate) fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), ConfigError> {
    use std::io::Write;

    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| ConfigError::io(dir, e))?;

    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config".to_string());
    let tmp = dir.join(format!(".{file_name}.{}.tmp", std::process::id()));

    let result = (|| -> io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(ConfigError::io(path, e));
    }
    Ok(())
}
