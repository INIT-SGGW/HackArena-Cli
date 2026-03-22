use crate::config::ensure_dir;
use crate::error::HackArenaError;
use flate2::read::GzDecoder;
use std::fs;
use std::io::{self};
use std::path::Path;
use tar::Archive as TarArchive;
use zip::ZipArchive;

/// Extracts an archive into `dest_dir`.
///
/// Supported formats:
/// - `.zip`
/// - `.tar.gz`
pub fn extract_archive(archive_path: &Path, dest_dir: &Path) -> Result<(), HackArenaError> {
    ensure_dir(dest_dir)?;

    let name = archive_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if name.ends_with(".zip") {
        extract_zip(archive_path, dest_dir)
    } else if name.ends_with(".tar.gz") {
        extract_tar_gz(archive_path, dest_dir)
    } else {
        Err(HackArenaError::archive_format(archive_path))
    }
}

fn extract_zip(path: &Path, dest_dir: &Path) -> Result<(), HackArenaError> {
    let file = fs::File::open(path).map_err(|e| HackArenaError::io_with_path(path, e))?;
    let mut zip = ZipArchive::new(file).map_err(|e| HackArenaError::zip_with_path(path, e))?;

    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| HackArenaError::zip_with_path(path, e))?;
        let outpath = match entry.enclosed_name() {
            Some(p) => dest_dir.join(p),
            None => continue,
        };

        if entry.is_dir() {
            ensure_dir(&outpath)?;
            continue;
        }

        if let Some(parent) = outpath.parent() {
            ensure_dir(parent)?;
        }

        let mut outfile =
            fs::File::create(&outpath).map_err(|e| HackArenaError::io_with_path(&outpath, e))?;
        io::copy(&mut entry, &mut outfile)
            .map_err(|e| HackArenaError::io_with_path(&outpath, e))?;
    }
    Ok(())
}

fn extract_tar_gz(path: &Path, dest_dir: &Path) -> Result<(), HackArenaError> {
    let file = fs::File::open(path).map_err(|e| HackArenaError::io_with_path(path, e))?;
    let gz = GzDecoder::new(file);
    let mut archive = TarArchive::new(gz);
    archive
        .unpack(dest_dir)
        .map_err(|e| HackArenaError::io_with_path(dest_dir, e))?;
    Ok(())
}

/// Removes a directory if it exists, then recreates it.
pub fn recreate_dir(path: &Path) -> Result<(), HackArenaError> {
    if path.exists() {
        fs::remove_dir_all(path).map_err(|e| HackArenaError::io_with_path(path, e))?;
    }
    ensure_dir(path)?;
    Ok(())
}

/// Marks a file as executable on Unix (best-effort).
pub fn ensure_executable(path: &Path) -> Result<(), HackArenaError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)
            .map_err(|e| HackArenaError::io_with_path(path, e))?
            .permissions();
        let mode = perms.mode();
        perms.set_mode(mode | 0o111);
        fs::set_permissions(path, perms).map_err(|e| HackArenaError::io_with_path(path, e))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}
