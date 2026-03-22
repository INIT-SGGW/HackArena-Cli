use crate::error::HackArenaError;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct EditionsRegistry {
    implemented: Vec<String>,
    allowed_unimplemented: Vec<String>,
}

fn registry() -> Result<EditionsRegistry, HackArenaError> {
    // This is intentionally embedded to keep the bootstrap CLI self-contained,
    // while making edition policy data-driven (no scattered hardcoding).
    const JSON: &str = r#"
{
  "implemented": ["3"],
  "allowed_unimplemented": ["1", "2", "2.5"]
}
"#;
    Ok(serde_json::from_str(JSON)
        .map_err(|e| HackArenaError::json_with_context("editions registry", e))?)
}

/// Validates that an edition exists and is supported by this CLI version.
pub fn validate_edition(edition: &str) -> Result<(), HackArenaError> {
    let reg = registry()?;
    let implemented_pretty = available_editions_pretty()?;
    if reg.implemented.iter().any(|e| e == edition) {
        return Ok(());
    }
    if reg.allowed_unimplemented.iter().any(|e| e == edition) {
        return Err(HackArenaError::msg(format!(
            "Edition `{edition}` is not implemented yet. Available editions: {implemented_pretty}."
        )));
    }
    Err(HackArenaError::msg(format!(
        "Edition `{edition}` does not exist. Available editions: {implemented_pretty}."
    )))
}

/// Returns a human-friendly list of editions supported by this CLI version.
pub fn available_editions_pretty() -> Result<String, HackArenaError> {
    let reg = registry()?;
    Ok(reg
        .implemented
        .iter()
        .map(|e| format!("`{e}`"))
        .collect::<Vec<_>>()
        .join(", "))
}
