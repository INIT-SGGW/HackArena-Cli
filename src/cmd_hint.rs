use std::path::Path;

pub fn run_cli(args: &str) -> String {
    let name = detect_cli_binary_name();
    format_run_cli_for_binary(&name, args, cfg!(windows))
}

fn format_run_cli_for_binary(name: &str, args: &str, windows: bool) -> String {
    let bin = if windows {
        if name.eq_ignore_ascii_case("hackarena.exe") {
            "hackarena.exe".to_string()
        } else {
            format!(".\\{name}")
        }
    } else {
        format!("./{name}")
    };
    if args.trim().is_empty() {
        maybe_quote(&bin)
    } else {
        format!("{} {}", maybe_quote(&bin), args.trim())
    }
}

fn detect_cli_binary_name() -> String {
    if let Ok(path) = std::env::current_exe()
        && let Some(name) = path.file_name().and_then(|s| s.to_str())
    {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            return Path::new(trimmed)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(trimmed)
                .to_string();
        }
    }
    if cfg!(windows) {
        "hackarena.exe".to_string()
    } else {
        "hackarena".to_string()
    }
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
    use super::format_run_cli_for_binary;

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
    fn quotes_binary_with_spaces() {
        let out = format_run_cli_for_binary("hack arena.exe", "doctor", true);
        assert_eq!(out, "\".\\hack arena.exe\" doctor");
    }
}
