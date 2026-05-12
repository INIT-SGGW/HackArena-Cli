use std::path::Path;

pub fn run_cli(args: &str) -> String {
    let invocation = detect_cli_invocation();
    format_run_cli_for_binary(&invocation, args, cfg!(windows))
}

fn format_run_cli_for_binary(invocation: &str, args: &str, windows: bool) -> String {
    let bin = if windows {
        if invocation.eq_ignore_ascii_case("hackarena.exe") {
            "hackarena.exe".to_string()
        } else if contains_path_separator(invocation) {
            invocation.replace('/', "\\")
        } else {
            format!(".\\{invocation}")
        }
    } else if contains_path_separator(invocation) {
        invocation.to_string()
    } else {
        format!("./{invocation}")
    };
    if args.trim().is_empty() {
        maybe_quote(&bin)
    } else {
        format!("{} {}", maybe_quote(&bin), args.trim())
    }
}

fn detect_cli_invocation() -> String {
    if let Some(arg0) = std::env::args_os()
        .next()
        .and_then(|value| value.into_string().ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        let cwd = std::env::current_dir().ok();
        return normalize_detected_invocation(&arg0, cwd.as_deref(), cfg!(windows));
    }

    if let Ok(path) = std::env::current_exe()
        && let Some(name) = path.file_name().and_then(|s| s.to_str())
    {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    if cfg!(windows) {
        "hackarena.exe".to_string()
    } else {
        "hackarena".to_string()
    }
}

fn normalize_detected_invocation(invocation: &str, cwd: Option<&Path>, windows: bool) -> String {
    if !contains_path_separator(invocation) {
        return invocation.to_string();
    }

    let path = Path::new(invocation);
    let Some(cwd) = cwd else {
        return invocation.to_string();
    };
    let Ok(invocation_abs) = absolutize_for_compare(path, cwd) else {
        return invocation.to_string();
    };
    let Ok(cwd_abs) = cwd.canonicalize() else {
        return invocation.to_string();
    };

    if invocation_abs.parent() == Some(cwd_abs.as_path())
        && let Some(file_name) = invocation_abs.file_name().and_then(|s| s.to_str())
    {
        return if windows {
            format!(".\\{file_name}")
        } else {
            format!("./{file_name}")
        };
    }

    invocation.to_string()
}

fn absolutize_for_compare(path: &Path, cwd: &Path) -> Result<std::path::PathBuf, std::io::Error> {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    joined.canonicalize()
}

fn contains_path_separator(value: &str) -> bool {
    value.contains('\\') || value.contains('/')
}

fn maybe_quote(value: &str) -> String {
    if value.contains(' ') {
        format!("\"{value}\"")
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{format_run_cli_for_binary, normalize_detected_invocation};
    use std::fs;

    #[test]
    fn formats_default_binary_hint() {
        let out = format_run_cli_for_binary("hackarena.exe", "status", cfg!(windows));
        if cfg!(windows) {
            assert_eq!(out, "hackarena.exe status");
        } else {
            assert_eq!(out, "./hackarena.exe status");
        }
    }

    #[test]
    fn formats_custom_binary_hint() {
        let out = format_run_cli_for_binary("hackarena-cli-v0.1.0-beta.1.exe", "install", true);
        assert_eq!(out, ".\\hackarena-cli-v0.1.0-beta.1.exe install");
    }

    #[test]
    fn preserves_explicit_relative_windows_invocation() {
        let out = format_run_cli_for_binary(".\\hackarena.exe", "status", true);
        assert_eq!(out, ".\\hackarena.exe status");
    }

    #[test]
    fn preserves_explicit_relative_unix_invocation() {
        let out = format_run_cli_for_binary("./hackarena", "status", false);
        assert_eq!(out, "./hackarena status");
    }

    #[test]
    fn quotes_binary_with_spaces() {
        let out = format_run_cli_for_binary("hack arena.exe", "doctor", true);
        assert_eq!(out, "\".\\hack arena.exe\" doctor");
    }

    #[test]
    fn normalizes_absolute_invocation_from_current_dir_on_windows() {
        let dir = tempfile::tempdir().expect("temp dir");
        let exe = dir.path().join("hackarena.exe");
        fs::write(&exe, b"").expect("exe placeholder");

        let normalized =
            normalize_detected_invocation(&exe.display().to_string(), Some(dir.path()), true);

        assert_eq!(normalized, ".\\hackarena.exe");
    }
}
