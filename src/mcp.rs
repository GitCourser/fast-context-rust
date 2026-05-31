use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::config::{is_env_set, read_bool_env, read_int_env, read_u64_env};
use crate::core::{SearchConfig, SearchEngine};
use crate::error::{FastContextError, FastContextErrorKind};
use crate::project_path::validate_project_path;

pub const FAST_CONTEXT_TOOL_NAME: &str = "fast_context_search";
const PROTOCOL_VERSION: &str = "2024-11-05";

/// Server-side runtime config sourced from FC_* environment variables.
///
/// Public MCP schema only exposes 7 task-level parameters. The remaining
/// strategy knobs (max_commands, timeout, repo_map_mode) live here and are
/// configured via env at server startup, so each tool invocation does not
/// pay the per-call schema token cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub max_turns: usize,
    pub max_commands: usize,
    pub timeout_ms: u64,
    pub max_results: usize,
    pub tree_depth: usize,
    pub repo_map_mode: String,
    pub include_snippets: bool,
    pub include_snippets_explicitly_set: bool,
}

impl RuntimeConfig {
    pub fn from_env() -> Self {
        let repo_map_mode = match std::env::var("FC_REPO_MAP_MODE")
            .ok()
            .as_deref()
            .map(str::trim)
        {
            Some("classic") => "classic".to_string(),
            _ => "bootstrap_hotspot".to_string(),
        };
        Self {
            max_turns: read_int_env("FC_MAX_TURNS", 3, 1, 5),
            max_commands: read_int_env("FC_MAX_COMMANDS", 8, 1, 20),
            timeout_ms: read_u64_env("FC_TIMEOUT_MS", 30_000, 1_000, 300_000),
            max_results: read_int_env("FC_MAX_RESULTS", 10, 1, 30),
            tree_depth: read_int_env("FC_TREE_DEPTH", 3, 0, 6),
            repo_map_mode,
            include_snippets: read_bool_env("FC_INCLUDE_SNIPPETS", false),
            include_snippets_explicitly_set: is_env_set("FC_INCLUDE_SNIPPETS"),
        }
    }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}

impl JsonRpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

pub fn tool_definition() -> Value {
    tool_definition_with(&RuntimeConfig::from_env())
}

pub fn tool_definition_with(config: &RuntimeConfig) -> Value {
    let snippets_default_text = if config.include_snippets_explicitly_set {
        format!(
            "Server-configured default is {} (FC_INCLUDE_SNIPPETS). Do NOT override unless the user explicitly asks for a different mode.",
            config.include_snippets
        )
    } else {
        "Default false (lightweight: file paths + line ranges + grep keywords). Set true for full code snippets.".to_string()
    };
    let include_snippets_description =
        format!("Include full code snippets. {snippets_default_text}");

    let description = "Bilingual (Chinese/English) semantic code search over a codebase. \
Use when code location is unknown, for natural-language business/logic queries, \
call-chain or business-flow understanding, cross-module/cross-layer tracing, and \
architecture research before a task. Returns relevant file paths with line ranges \
and follow-up grep keywords.";

    json!({
        "name": FAST_CONTEXT_TOOL_NAME,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Natural-language query in Chinese or English (e.g. 'XX部署流程', 'XX事件处理', 'trace router to service to model')."
                },
                "project_path": {
                    "type": "string",
                    "description": "Absolute path to the project root directory."
                },
                "tree_depth": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 6,
                    "default": config.tree_depth,
                    "description": "Directory tree depth for repo map (0-6, 0 = auto). Auto falls back to lower depth if tree exceeds 250KB."
                },
                "max_turns": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 5,
                    "default": config.max_turns,
                    "description": "Search rounds (1-5). More = deeper search but slower."
                },
                "max_results": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 30,
                    "default": config.max_results,
                    "description": "Maximum number of files to return (1-30)."
                },
                "exclude_paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "default": [],
                    "description": "Directory/file patterns to exclude from repo map and search context. Useful for large repos or noisy generated files."
                },
                "include_code_snippets": {
                    "type": "boolean",
                    "default": config.include_snippets,
                    "description": include_snippets_description
                }
            },
            "required": ["query", "project_path"]
        }
    })
}

pub fn tools_list_result() -> Value {
    json!({ "tools": [tool_definition()] })
}

pub async fn call_fast_context_search(args: &Value) -> Value {
    call_fast_context_search_with(args, &RuntimeConfig::from_env()).await
}

pub async fn call_fast_context_search_with(args: &Value, runtime: &RuntimeConfig) -> Value {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let project_path = args.get("project_path").and_then(Value::as_str);
    let max_turns = args
        .get("max_turns")
        .and_then(Value::as_u64)
        .map(|value| (value as usize).clamp(1, 5))
        .unwrap_or(runtime.max_turns);
    let max_results = args
        .get("max_results")
        .and_then(Value::as_u64)
        .map(|value| (value as usize).clamp(1, 30))
        .unwrap_or(runtime.max_results);
    let tree_depth = args
        .get("tree_depth")
        .and_then(Value::as_u64)
        .map(|value| (value as usize).clamp(0, 6))
        .unwrap_or(runtime.tree_depth);
    let include_code_snippets = args
        .get("include_code_snippets")
        .and_then(Value::as_bool)
        .unwrap_or(runtime.include_snippets);
    let exclude_paths = args
        .get("exclude_paths")
        .and_then(Value::as_array)
        .map(|paths| {
            paths
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if query.trim().is_empty() {
        return tool_error_text(
            None,
            &FastContextError::new(FastContextErrorKind::InvalidResponse, "missing query"),
        );
    }

    if let Some(error) = validate_project_path(project_path) {
        return tool_error_text(
            None,
            &FastContextError::new(FastContextErrorKind::InvalidResponse, error),
        );
    }
    let project_path = project_path.expect("validated project_path must exist");

    let mut config = SearchConfig::new(PathBuf::from(project_path));
    config.max_rounds = max_turns;
    config.max_results = max_results;
    config.tree_depth = tree_depth;
    config.include_code_snippets = include_code_snippets;
    config.exclude_paths = exclude_paths;
    config.max_commands = runtime.max_commands;
    config.timeout_ms = runtime.timeout_ms;
    config.repo_map_mode = runtime.repo_map_mode.clone();
    let engine = SearchEngine::new(config);
    let metadata = engine.metadata();

    match engine.search(&query).await {
        Ok(text) => json!({
            "content": [{ "type": "text", "text": text }]
        }),
        Err(error) => tool_error_text(Some(&metadata), &error),
    }
}

fn tool_error_text(metadata: Option<&str>, error: &FastContextError) -> Value {
    let text = match metadata {
        Some(metadata) => format!("{metadata}\n{}", error.user_message()),
        None => error.user_message(),
    };
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": true
    })
}

pub async fn handle_json_rpc_message(message: &Value) -> Option<Value> {
    let method = message.get("method").and_then(Value::as_str)?;
    let id = message.get("id").cloned();

    if method == "notifications/initialized" {
        return None;
    }

    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {
                "tools": { "listChanged": false }
            },
            "serverInfo": {
                "name": "fast-context-rust",
                "version": env!("CARGO_PKG_VERSION")
            }
        })),
        "tools/list" => Ok(tools_list_result()),
        "tools/call" => {
            let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if name != FAST_CONTEXT_TOOL_NAME {
                Err(JsonRpcError::new(
                    -32602,
                    format!("unknown tool: {name}; only {FAST_CONTEXT_TOOL_NAME} is supported"),
                ))
            } else {
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                Ok(call_fast_context_search(&args).await)
            }
        }
        _ => Err(JsonRpcError::new(
            -32601,
            format!("method not found: {method}"),
        )),
    };

    id.map(|id| match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": error.code, "message": error.message }
        }),
    })
}

pub async fn serve_stdio() -> Result<(), FastContextError> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = line.map_err(|error| {
            FastContextError::new(
                FastContextErrorKind::NetworkError,
                format!("failed to read MCP stdin: {error}"),
            )
        })?;
        if line.trim().is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle_json_rpc_message(&message).await,
            Err(error) => Some(json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": { "code": -32700, "message": format!("parse error: {error}") }
            })),
        };

        if let Some(response) = response {
            writeln!(stdout, "{response}").map_err(|error| {
                FastContextError::new(
                    FastContextErrorKind::NetworkError,
                    format!("failed to write MCP stdout: {error}"),
                )
            })?;
            stdout.flush().map_err(|error| {
                FastContextError::new(
                    FastContextErrorKind::NetworkError,
                    format!("failed to flush MCP stdout: {error}"),
                )
            })?;
        }
    }

    Ok(())
}
