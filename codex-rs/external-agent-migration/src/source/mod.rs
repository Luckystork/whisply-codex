mod cla;
mod cur;

use crate::invalid_data_error;
use serde_json::Value as JsonValue;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use toml::Value as TomlValue;

pub use cla::ClaSource;
pub use cur::CurSource;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionSourceGroup {
    pub scope: PathBuf,
    pub sources: Vec<PathBuf>,
}

fn read_json_file(path: &Path) -> io::Result<Option<JsonValue>> {
    if !path.is_file() {
        return Ok(None);
    }

    let raw = fs::read_to_string(path)?;
    let value = serde_json::from_str(&raw).map_err(|err| invalid_data_error(err.to_string()))?;
    Ok(Some(value))
}

fn build_config(
    settings: &JsonValue,
    append_source_config: fn(
        &mut toml::map::Map<String, TomlValue>,
        &serde_json::Map<String, JsonValue>,
    ),
) -> io::Result<TomlValue> {
    let Some(settings) = settings.as_object() else {
        return Err(invalid_data_error(
            "external agent settings root must be an object",
        ));
    };

    let mut root = toml::map::Map::new();
    append_source_config(&mut root, settings);
    Ok(TomlValue::Table(root))
}

fn is_non_empty_text_file(path: &Path) -> io::Result<bool> {
    if !path.is_file() {
        return Ok(false);
    }
    Ok(!fs::read_to_string(path)?.trim().is_empty())
}
