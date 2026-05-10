use crate::config::{
    Paths, is_project_dir, load_project_manifest, project_config_path, project_manifest_path,
    project_meta_dir,
};
use crate::error::HackArenaError;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub async fn clean(
    paths: &Paths,
    all: bool,
    project: bool,
    global: bool,
    force: bool,
    save: bool,
) -> Result<(), HackArenaError> {
    let cwd = std::env::current_dir().map_err(HackArenaError::Io)?;

    let selected_project = all || project || (!project && !global);
    let selected_global = all || global || (!project && !global);

    let mut items: Vec<CleanItem> = Vec::new();

    // Project-local
    if selected_project && is_project_dir(&cwd) {
        if let Ok(m) = load_project_manifest(&cwd) {
            if let Some(backend) = m.backend.as_ref() {
                items.push(CleanItem::dir(
                    "project backend",
                    cwd.join(&backend.install_dir),
                    backend.installed_at_unix,
                ));
            }

            if let Some(standalone) = m.standalone.as_ref() {
                items.push(CleanItem::dir(
                    "project standalone",
                    cwd.join(&standalone.install_dir),
                    standalone.installed_at_unix,
                ));
            }

            for (wrapper_id, wrapper) in &m.wrappers {
                items.push(CleanItem::dir(
                    &format!("project wrapper `{wrapper_id}`"),
                    cwd.join(&wrapper.install_dir),
                    wrapper.installed_at_unix,
                ));
            }
        }

        items.push(CleanItem::dir(
            "project meta dir",
            project_meta_dir(&cwd),
            None,
        ));
        let manifest_path = project_manifest_path(&cwd);
        items.push(CleanItem::file("project manifest", manifest_path, None));

        let project_path = project_config_path(&cwd);
        items.push(CleanItem::file("project config", project_path, None));
    }

    // Global
    if selected_global {
        // Prefer leaf deletes (auth binary) over deleting entire dirs.
        for auth_name in ["ha-auth.exe", "ha-auth"] {
            items.push(CleanItem::file(
                "global ha-auth",
                paths.bin_dir.join(auth_name),
                None,
            ));
        }

        items.push(CleanItem::dir(
            "global downloads cache",
            paths.downloads_cache_dir(),
            None,
        ));
        items.push(CleanItem::dir("global logs", paths.logs_dir(), None));
        items.push(CleanItem::dir("global bin", paths.bin_dir.clone(), None));
        items.push(CleanItem::dir("global data root", paths.data_root(), None));
        items.push(CleanItem::dir(
            "global config root",
            paths.config_root(),
            None,
        ));
    }

    // De-dupe and order by (scope, destructiveness, label).
    items.sort_by(|a, b| {
        let ka = sort_key(&a.label);
        let kb = sort_key(&b.label);
        ka.cmp(&kb).then_with(|| a.path.cmp(&b.path))
    });
    items.dedup_by(|a, b| a.path == b.path);

    if items.is_empty() {
        println!("Nothing to clean.");
        return Ok(());
    }

    println!("hackarena clean");
    println!();
    println!("This command removes files/directories from your project and HackArena global dirs.");
    println!(
        "Scope: {}{}",
        if selected_project { "project" } else { "" },
        if selected_global { "+global" } else { "" }
    );
    println!(
        "Use `--force` to skip prompts. Use `--save` to skip items modified after install time."
    );
    println!();

    let mut declined: Vec<PathBuf> = Vec::new();

    for item in items {
        if !item.path.exists() {
            continue;
        }

        // If the user declined deleting something inside this directory earlier,
        // do not ask about deleting the parent directory.
        if item.path.is_dir() && declined.iter().any(|p| p.starts_with(&item.path)) {
            println!(
                "Skip {} (contains items you chose to keep): {}",
                item.label,
                item.path.display()
            );
            continue;
        }

        if save {
            let Some(installed_at) = item.installed_at_unix else {
                println!(
                    "Skip {} (no install timestamp): {}",
                    item.label,
                    item.path.display()
                );
                continue;
            };
            if is_modified_since(&item.path, installed_at)? {
                println!(
                    "Skip {} (modified after install): {}",
                    item.label,
                    item.path.display()
                );
                continue;
            }
        }

        if !force {
            if !confirm(&format!(
                "Delete {} at {}?",
                item.label,
                item.path.display()
            ))? {
                declined.push(item.path.clone());
                continue;
            }
        }

        if let Err(err) = delete_path(&item.path) {
            print_clean_error(&item.label, &item.path, &err);
            continue;
        }
        println!("Deleted {}: {}", item.label, item.path.display());

        // Prune empty parent directories for known roots.
        if item.label.starts_with("project ") {
            let stop = &cwd;
            if let Err(err) = prune_empty_parents(item.path.parent().unwrap_or(&cwd), stop) {
                print_clean_error(
                    "project parent cleanup",
                    item.path.parent().unwrap_or(&cwd),
                    &err,
                );
            }
        } else if item.label.starts_with("global ") {
            // Don't climb outside the HackArena roots.
            if let Err(err) = prune_empty_parents(&paths.bin_dir, &paths.bin_dir) {
                print_clean_error("global parent cleanup", &paths.bin_dir, &err);
            }
            if let Err(err) =
                prune_empty_parents(&paths.downloads_cache_dir(), &paths.downloads_cache_dir())
            {
                print_clean_error("global parent cleanup", &paths.downloads_cache_dir(), &err);
            }
            if let Err(err) = prune_empty_parents(&paths.data_root(), &paths.data_root()) {
                print_clean_error("global parent cleanup", &paths.data_root(), &err);
            }
            if let Err(err) = prune_empty_parents(&paths.config_root(), &paths.config_root()) {
                print_clean_error("global parent cleanup", &paths.config_root(), &err);
            }
        }
    }

    // Finally, offer to remove now-empty roots (only if we didn't keep something inside).
    if selected_project {
        let meta = project_meta_dir(&cwd);
        if meta.exists() && is_dir_empty(&meta)? && !declined.iter().any(|p| p.starts_with(&meta)) {
            if let Err(err) = maybe_delete_empty_dir("project meta dir", &meta, force) {
                print_clean_error("project meta dir", &meta, &err);
            }
        }
    }

    if selected_global {
        // On Windows this is typically `%LOCALAPPDATA%\\HackArena`.
        // On Unix it's typically `<data_root>/..` (e.g. `~/.local/share/hackarena`).
        if let Some(global_root) = paths.bin_dir.parent().map(Path::to_path_buf) {
            if global_root.exists()
                && is_dir_empty(&global_root)?
                && !declined.iter().any(|p| p.starts_with(&global_root))
            {
                if let Err(err) =
                    maybe_delete_empty_dir("global HackArena root", &global_root, force)
                {
                    print_clean_error("global HackArena root", &global_root, &err);
                }
            }
        }
    }

    Ok(())
}

fn sort_key(label: &str) -> (u8, u8, String) {
    // 1) project before global
    // 2) less destructive before more destructive
    let scope = if label.starts_with("project ") { 0 } else { 1 };
    let destructiveness = if label.contains("manifest") {
        2
    } else if label.contains("config") {
        3
    } else if label.contains("wrapper") {
        1
    } else if label.contains("backend") {
        1
    } else if label.contains("standalone") {
        1
    } else if label.contains("downloads cache") {
        0
    } else if label.contains("logs") {
        0
    } else if label.contains("bin") {
        2
    } else if label.contains("data root") {
        4
    } else if label.contains("config root") {
        4
    } else {
        5
    };
    (scope, destructiveness, label.to_string())
}

#[derive(Debug, Clone)]
struct CleanItem {
    label: String,
    path: PathBuf,
    installed_at_unix: Option<u64>,
}

impl CleanItem {
    fn file(label: &str, path: PathBuf, installed_at_unix: Option<u64>) -> Self {
        Self {
            label: label.to_string(),
            path,
            installed_at_unix,
        }
    }

    fn dir(label: &str, path: PathBuf, installed_at_unix: Option<u64>) -> Self {
        Self {
            label: label.to_string(),
            path,
            installed_at_unix,
        }
    }
}

fn confirm(question: &str) -> Result<bool, HackArenaError> {
    let mut stdout = io::stdout();
    write!(&mut stdout, "{question} [y/N]: ").map_err(HackArenaError::Io)?;
    stdout.flush().map_err(HackArenaError::Io)?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(HackArenaError::Io)?;
    let trimmed = input.trim().to_ascii_lowercase();
    Ok(trimmed == "y" || trimmed == "yes")
}

fn delete_path(path: &Path) -> Result<(), HackArenaError> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_dir() {
        fs::remove_dir_all(path).map_err(|e| HackArenaError::io_with_path(path, e))?;
    } else {
        fs::remove_file(path).map_err(|e| HackArenaError::io_with_path(path, e))?;
    }
    Ok(())
}

fn maybe_delete_empty_dir(label: &str, path: &Path, force: bool) -> Result<(), HackArenaError> {
    if !path.exists() {
        return Ok(());
    }
    if !is_dir_empty(path)? {
        return Ok(());
    }
    if !force && !confirm(&format!("Delete empty {} at {}?", label, path.display()))? {
        return Ok(());
    }
    fs::remove_dir(path).map_err(|e| HackArenaError::io_with_path(path, e))?;
    println!("Deleted empty {}: {}", label, path.display());
    Ok(())
}

fn is_dir_empty(path: &Path) -> Result<bool, HackArenaError> {
    if !path.exists() {
        return Ok(true);
    }
    if !path.is_dir() {
        return Ok(false);
    }
    Ok(path
        .read_dir()
        .map_err(|e| HackArenaError::io_with_path(path, e))?
        .next()
        .is_none())
}

fn prune_empty_parents(path: &Path, stop_at: &Path) -> Result<(), HackArenaError> {
    let mut current = path.to_path_buf();
    loop {
        if !current.starts_with(stop_at) {
            break;
        }
        if current == stop_at {
            break;
        }
        if !current.exists() {
            if let Some(parent) = current.parent() {
                current = parent.to_path_buf();
                continue;
            }
            break;
        }
        if current.is_dir()
            && current
                .read_dir()
                .map_err(|e| HackArenaError::io_with_path(&current, e))?
                .next()
                .is_none()
        {
            fs::remove_dir(&current).map_err(|e| HackArenaError::io_with_path(&current, e))?;
            if let Some(parent) = current.parent() {
                current = parent.to_path_buf();
                continue;
            }
            break;
        }
        break;
    }
    Ok(())
}

fn is_modified_since(path: &Path, installed_at_unix: u64) -> Result<bool, HackArenaError> {
    let installed = UNIX_EPOCH + std::time::Duration::from_secs(installed_at_unix);
    is_modified_since_inner(path, installed)
}

fn is_modified_since_inner(path: &Path, installed: SystemTime) -> Result<bool, HackArenaError> {
    let meta = fs::metadata(path).map_err(|e| HackArenaError::io_with_path(path, e))?;
    if meta.is_file() {
        if let Ok(mtime) = meta.modified() {
            return Ok(mtime > installed);
        }
        return Ok(false);
    }

    let ignore_names = [".git", "target", "node_modules", ".idea", "__pycache__"];

    let rd = fs::read_dir(path).map_err(|e| HackArenaError::io_with_path(path, e))?;
    for entry in rd {
        let entry = entry.map_err(HackArenaError::Io)?;
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if ignore_names.iter().any(|n| n == &file_name) {
            continue;
        }
        let p = entry.path();
        if is_modified_since_inner(&p, installed)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn print_clean_error(label: &str, path: &Path, err: &HackArenaError) {
    eprintln!("Skip {} at {}: {}", label, path.display(), err);
}
