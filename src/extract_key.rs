use std::env;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::Value;

use crate::error::{FastContextError, FastContextErrorKind};

pub fn default_windsurf_db_path() -> Result<PathBuf, FastContextError> {
    let home = env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
        FastContextError::new(FastContextErrorKind::InvalidResponse, "HOME is not set")
    })?;

    let path = match env::consts::OS {
        "macos" => home.join("Library/Application Support/Windsurf/User/globalStorage/state.vscdb"),
        "windows" => {
            let appdata = env::var_os("APPDATA").map(PathBuf::from).ok_or_else(|| {
                FastContextError::new(FastContextErrorKind::InvalidResponse, "APPDATA is not set")
            })?;
            appdata.join("Windsurf/User/globalStorage/state.vscdb")
        }
        _ => {
            let config = env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"));
            config.join("Windsurf/User/globalStorage/state.vscdb")
        }
    };

    Ok(path)
}

pub fn extract_key_from_db_path(path: impl AsRef<Path>) -> Result<String, FastContextError> {
    let path = path.as_ref();
    if !path.exists() {
        return Err(FastContextError::new(
            FastContextErrorKind::MissingApiKey,
            format!("Windsurf database not found: {}", path.display()),
        ));
    }

    let connection = Connection::open(path).map_err(|error| {
        FastContextError::new(
            FastContextErrorKind::InvalidResponse,
            format!("failed to open Windsurf database: {error}"),
        )
    })?;

    for query in candidate_queries(&connection)? {
        let mut statement = match connection.prepare(&query) {
            Ok(statement) => statement,
            Err(_) => continue,
        };
        let rows = match statement.query_map([], |row| row.get::<_, String>(0)) {
            Ok(rows) => rows,
            Err(_) => continue,
        };
        for value in rows.flatten() {
            if let Some(key) = extract_key_from_text(&value) {
                return Ok(key);
            }
        }
    }

    Err(FastContextError::new(
        FastContextErrorKind::MissingApiKey,
        "Windsurf API key was not found in known Windsurf SQLite records",
    ))
}

fn candidate_queries(connection: &Connection) -> Result<Vec<String>, FastContextError> {
    let mut queries = vec![
        "SELECT value FROM ItemTable WHERE key = 'windsurfAuthStatus'".to_string(),
        "SELECT value FROM ItemTable WHERE lower(key) LIKE '%windsurf%' AND (lower(key) LIKE '%auth%' OR lower(key) LIKE '%api%' OR lower(key) LIKE '%token%')".to_string(),
    ];

    let mut statement = connection
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .map_err(|error| {
            FastContextError::new(
                FastContextErrorKind::InvalidResponse,
                format!("failed to inspect SQLite schema: {error}"),
            )
        })?;
    let tables = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| {
            FastContextError::new(
                FastContextErrorKind::InvalidResponse,
                format!("failed to list SQLite tables: {error}"),
            )
        })?;

    for table in tables.flatten() {
        if !is_safe_identifier(&table) {
            continue;
        }
        let columns = table_columns(connection, &table);
        if columns.is_empty() {
            continue;
        }

        let table_is_windsurf_specific = table.to_ascii_lowercase().contains("windsurf");
        if table_is_windsurf_specific {
            for column in columns
                .iter()
                .filter(|column| is_interesting_column(column))
            {
                queries.push(format!(
                    "SELECT {column} FROM {table} WHERE {column} IS NOT NULL"
                ));
            }
        }

        if has_column(&columns, "key") {
            for value_column in columns
                .iter()
                .filter(|column| is_interesting_column(column))
            {
                if value_column.eq_ignore_ascii_case("key") {
                    continue;
                }
                queries.push(format!(
                    "SELECT {value_column} FROM {table} WHERE lower(key) LIKE '%windsurf%' AND {value_column} IS NOT NULL"
                ));
            }
        }
    }

    Ok(queries)
}

fn table_columns(connection: &Connection, table: &str) -> Vec<String> {
    let pragma = format!("PRAGMA table_info({table})");
    let mut column_statement = match connection.prepare(&pragma) {
        Ok(statement) => statement,
        Err(_) => return Vec::new(),
    };
    let columns = match column_statement.query_map([], |row| row.get::<_, String>(1)) {
        Ok(columns) => columns,
        Err(_) => return Vec::new(),
    };
    columns
        .flatten()
        .filter(|column| is_safe_identifier(column))
        .collect()
}

fn extract_key_from_text(text: &str) -> Option<String> {
    if let Ok(value) = serde_json::from_str::<Value>(text)
        && let Some(key) = find_api_key_json(&value)
    {
        return Some(key);
    }

    for marker in ["windsurfApiKey", "apiKey", "api_key", "token"] {
        if let Some(index) = text.find(marker) {
            let tail = &text[index + marker.len()..];
            let candidate: String = tail
                .chars()
                .skip_while(|ch| !ch.is_ascii_alphanumeric())
                .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
                .collect();
            if candidate.len() >= 12 {
                return Some(candidate);
            }
        }
    }

    None
}

fn find_api_key_json(value: &Value) -> Option<String> {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                let normalized = key.to_ascii_lowercase().replace(['_', '-'], "");
                if matches!(normalized.as_str(), "apikey" | "windsurfapikey" | "token")
                    && let Some(text) = value.as_str()
                    && text.len() >= 12
                {
                    return Some(text.to_string());
                }
                if let Some(found) = find_api_key_json(value) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(values) => values.iter().find_map(find_api_key_json),
        _ => None,
    }
}

fn is_safe_identifier(identifier: &str) -> bool {
    !identifier.is_empty()
        && identifier
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn has_column(columns: &[String], expected: &str) -> bool {
    columns
        .iter()
        .any(|column| column.eq_ignore_ascii_case(expected))
}

fn is_interesting_column(column: &str) -> bool {
    let lower = column.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "value" | "data" | "json" | "body" | "token" | "apikey" | "api_key"
    ) || lower.contains("auth")
        || lower.contains("api")
        || lower.contains("token")
}
