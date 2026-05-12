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
        return arg0;
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
}
