use crate::error::{AppError, AppResult, ReviewErrorCode};
use serde::Deserialize;
use std::{
    fs::{self, File},
    io::{self, Cursor, Read, Write},
    path::{Component, Path, PathBuf},
};
use zip::ZipArchive;

const DEFAULT_MAX_ARCHIVE_BYTES: usize = 100 * 1024 * 1024;
const DEFAULT_MAX_EXTRACTED_FILES: usize = 10_000;
const DEFAULT_MAX_EXTRACTED_BYTES: usize = 200 * 1024 * 1024;
const DEFAULT_MAX_SINGLE_FILE_BYTES: usize = 10 * 1024 * 1024;
const DEFAULT_MAX_ENTRY_PATH_BYTES: usize = 512;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ArchiveLimits {
    #[serde(default = "default_max_archive_bytes")]
    pub max_archive_bytes: usize,
    #[serde(default = "default_max_extracted_files")]
    pub max_extracted_files: usize,
    #[serde(default = "default_max_extracted_bytes")]
    pub max_extracted_bytes: usize,
    #[serde(default = "default_max_single_file_bytes")]
    pub max_single_file_bytes: usize,
    #[serde(default = "default_max_entry_path_bytes")]
    pub max_entry_path_bytes: usize,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            max_archive_bytes: default_max_archive_bytes(),
            max_extracted_files: default_max_extracted_files(),
            max_extracted_bytes: default_max_extracted_bytes(),
            max_single_file_bytes: default_max_single_file_bytes(),
            max_entry_path_bytes: default_max_entry_path_bytes(),
        }
    }
}

fn default_max_archive_bytes() -> usize {
    DEFAULT_MAX_ARCHIVE_BYTES
}

fn default_max_extracted_files() -> usize {
    DEFAULT_MAX_EXTRACTED_FILES
}

fn default_max_extracted_bytes() -> usize {
    DEFAULT_MAX_EXTRACTED_BYTES
}

fn default_max_single_file_bytes() -> usize {
    DEFAULT_MAX_SINGLE_FILE_BYTES
}

fn default_max_entry_path_bytes() -> usize {
    DEFAULT_MAX_ENTRY_PATH_BYTES
}

pub(crate) fn extract_zip_archive(
    bytes: &[u8],
    destination: &Path,
    limits: &ArchiveLimits,
) -> AppResult<usize> {
    if bytes.len() > limits.max_archive_bytes {
        return Err(AppError::archive(
            ReviewErrorCode::ArchiveLimitExceeded,
            format!(
                "repository archive size {} exceeded max_archive_bytes {}",
                bytes.len(),
                limits.max_archive_bytes
            ),
        ));
    }
    let reader = Cursor::new(bytes);
    let mut archive = ZipArchive::new(reader)
        .map_err(|err| AppError::archive(ReviewErrorCode::ArchiveExtractFailed, err.to_string()))?;
    let mut extracted_files = 0_usize;
    let mut extracted_bytes = 0_usize;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).map_err(|err| {
            AppError::archive(ReviewErrorCode::ArchiveExtractFailed, err.to_string())
        })?;
        let Some(path) = file.enclosed_name() else {
            continue;
        };
        let relative = strip_first_component(&path);
        if relative.as_os_str().is_empty() {
            continue;
        }
        let relative_text = relative.to_string_lossy();
        if relative_text.len() > limits.max_entry_path_bytes {
            return Err(AppError::archive(
                ReviewErrorCode::ArchiveLimitExceeded,
                format!(
                    "archive entry path length {} exceeded max_entry_path_bytes {}: {}",
                    relative_text.len(),
                    limits.max_entry_path_bytes,
                    relative_text
                ),
            ));
        }
        let output_path = destination.join(relative);
        if file.is_dir() {
            fs::create_dir_all(&output_path)?;
            continue;
        }
        if extracted_files >= limits.max_extracted_files {
            return Err(AppError::archive(
                ReviewErrorCode::ArchiveLimitExceeded,
                format!(
                    "archive extracted file count exceeded max_extracted_files {}",
                    limits.max_extracted_files
                ),
            ));
        }
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = File::create(&output_path)?;
        let copied = copy_zip_file_with_limits(
            &mut file,
            &mut output,
            limits.max_single_file_bytes,
            limits.max_extracted_bytes.saturating_sub(extracted_bytes),
        )
        .inspect_err(|_| {
            let _ = fs::remove_file(&output_path);
        })?;
        extracted_bytes = extracted_bytes.saturating_add(copied);
        set_unix_mode(&output_path, file.unix_mode())?;
        extracted_files += 1;
    }
    Ok(extracted_files)
}

fn copy_zip_file_with_limits<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    max_single_file_bytes: usize,
    remaining_total_bytes: usize,
) -> AppResult<usize> {
    let mut copied = 0_usize;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(copied);
        }
        if copied.saturating_add(read) > max_single_file_bytes {
            return Err(AppError::archive(
                ReviewErrorCode::ArchiveLimitExceeded,
                format!(
                    "archive file exceeded max_single_file_bytes {}",
                    max_single_file_bytes
                ),
            ));
        }
        if read > remaining_total_bytes.saturating_sub(copied) {
            return Err(AppError::archive(
                ReviewErrorCode::ArchiveLimitExceeded,
                "archive extracted bytes exceeded max_extracted_bytes",
            ));
        }
        writer.write_all(&buffer[..read])?;
        copied += read;
    }
}

fn strip_first_component(path: &Path) -> PathBuf {
    path.components()
        .skip(1)
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value),
            _ => None,
        })
        .collect()
}

#[cfg(unix)]
fn set_unix_mode(path: &Path, mode: Option<u32>) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if let Some(mode) = mode {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_unix_mode(_path: &Path, _mode: Option<u32>) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive_with_entry(name: &str, content: &[u8]) -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut bytes);
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(content).unwrap();
            zip.finish().unwrap();
        }
        bytes.into_inner()
    }

    fn test_archive() -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut bytes);
            for name in ["repo-head/README.md", "repo-head/src/lib.rs"] {
                zip.start_file(name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                zip.write_all(b"test\n").unwrap();
            }
            zip.finish().unwrap();
        }
        bytes.into_inner()
    }

    fn permissive_archive_limits() -> ArchiveLimits {
        ArchiveLimits {
            max_archive_bytes: usize::MAX,
            max_extracted_files: usize::MAX,
            max_extracted_bytes: usize::MAX,
            max_single_file_bytes: usize::MAX,
            max_entry_path_bytes: usize::MAX,
        }
    }

    #[test]
    fn extract_zip_archive_rejects_archive_over_limit() {
        let temp = tempfile::tempdir().unwrap();
        let archive = test_archive();
        let limits = ArchiveLimits {
            max_archive_bytes: archive.len() - 1,
            ..permissive_archive_limits()
        };

        let err = extract_zip_archive(&archive, temp.path(), &limits).unwrap_err();

        assert!(err.to_string().contains("max_archive_bytes"));
    }

    #[test]
    fn extract_zip_archive_rejects_too_many_files() {
        let temp = tempfile::tempdir().unwrap();
        let limits = ArchiveLimits {
            max_extracted_files: 1,
            ..permissive_archive_limits()
        };

        let err = extract_zip_archive(&test_archive(), temp.path(), &limits).unwrap_err();

        assert!(err.to_string().contains("max_extracted_files"));
    }

    #[test]
    fn extracted_bytes_limit_returns_structured_archive_error() {
        let temp = tempfile::tempdir().unwrap();
        let archive = archive_with_entry("repo-head/src/lib.rs", b"12345");
        let limits = ArchiveLimits {
            max_extracted_bytes: 4,
            ..permissive_archive_limits()
        };

        let err = extract_zip_archive(&archive, temp.path(), &limits).unwrap_err();

        assert_eq!(
            err.review_failure().map(|failure| failure.code),
            Some(ReviewErrorCode::ArchiveLimitExceeded)
        );
        assert!(err.to_string().contains("max_extracted_bytes"));
        assert!(!temp.path().join("src/lib.rs").exists());
    }

    #[test]
    fn extract_zip_archive_rejects_single_file_over_limit() {
        let temp = tempfile::tempdir().unwrap();
        let archive = archive_with_entry("repo-head/src/lib.rs", b"12345");
        let limits = ArchiveLimits {
            max_single_file_bytes: 4,
            ..permissive_archive_limits()
        };

        let err = extract_zip_archive(&archive, temp.path(), &limits).unwrap_err();

        assert!(err.to_string().contains("max_single_file_bytes"));
        assert!(!temp.path().join("src/lib.rs").exists());
    }

    #[test]
    fn extract_zip_archive_rejects_entry_path_over_limit() {
        let temp = tempfile::tempdir().unwrap();
        let archive = archive_with_entry("repo-head/src/deep/lib.rs", b"content");
        let limits = ArchiveLimits {
            max_entry_path_bytes: "src/lib.rs".len(),
            ..permissive_archive_limits()
        };

        let err = extract_zip_archive(&archive, temp.path(), &limits).unwrap_err();

        assert!(err.to_string().contains("max_entry_path_bytes"));
    }
}
