use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use regex::Regex;
use reqwest::header::{self, HeaderMap, HeaderValue};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::error::{FastContextError, FastContextErrorKind, classify_http_status};
use crate::executor::ToolExecutor;
use crate::protobuf::{
    ProtobufEncoder, connect_frame_decode, connect_frame_encode, extract_strings,
};

const API_BASE: &str = "https://server.self-serve.windsurf.com/exa.api_server_pb.ApiServerService";
const AUTH_BASE: &str = "https://server.self-serve.windsurf.com/exa.auth_pb.AuthService";
const WS_APP: &str = "windsurf";
const DEFAULT_WS_APP_VER: &str = "1.48.2";
const DEFAULT_WS_LS_VER: &str = "1.9544.35";
const DEFAULT_MODEL: &str = "MODEL_SWE_1_6_FAST";
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const MAX_TREE_BYTES: usize = 250 * 1024;
const FINAL_FORCE_ANSWER: &str =
    "You have no turns left. Now you MUST provide your final ANSWER, even if it's not complete.";

const DEFAULT_EXCLUDE_PATHS: &[&str] = &[
    "node_modules",
    "vendor",
    ".venv",
    "venv",
    ".git",
    ".svn",
    ".hg",
    "dist",
    "build",
    "out",
    "target",
    ".next",
    ".nuxt",
    ".output",
    "__pycache__",
    ".cache",
    ".pytest_cache",
    "*.min.*",
    "coverage",
    ".idea",
    ".vscode",
];

const SYSTEM_PROMPT_TEMPLATE: &str = r#"You are an expert software engineer, responsible for providing context to another engineer to solve a code issue in the current codebase.

# IMPORTANT
- Return file paths and line ranges that contain ALL information relevant to understand and correctly address the issue.
- Include complete semantic blocks where possible, not isolated lines.
- Prefer fewer, higher-signal files over broad irrelevant context.

# ENVIRONMENT
- Working directory: /codebase.
- Tool access: use the restricted_exec tool ONLY.
- Allowed sub-commands: rg, readfile, tree, ls, glob.

# TOOL USE GUIDELINES
- Use a SINGLE restricted_exec call per turn with at most {max_commands} commands.
- You have at most {max_turns} search turns, so batch useful commands.
- Prefer narrow rg/readfile/tree commands, with aggressive excludes for large generated or dependency directories.

# ANSWER FORMAT
- Final output MUST call the answer tool with XML in the answer argument.
- XML format:
<ANSWER>
  <file path="/codebase/path/to/file.rs">
    <range>10-60</range>
  </file>
</ANSWER>
- Aim to return at most {max_results} files. If no relevant files exist, return <ANSWER></ANSWER>.
"#;

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub thinking: String,
    pub name: String,
    pub args: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswerFileRange {
    pub path: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchConfig {
    pub project_path: PathBuf,
    pub api_key: Option<String>,
    pub model: String,
    pub max_rounds: usize,
    pub max_results: usize,
    pub max_commands: usize,
    pub tree_depth: usize,
    pub timeout_ms: u64,
    pub exclude_paths: Vec<String>,
    pub include_code_snippets: bool,
    pub repo_map_mode: String,
}

impl SearchConfig {
    #[must_use]
    pub fn new(project_path: impl Into<PathBuf>) -> Self {
        Self {
            project_path: project_path.into(),
            api_key: std::env::var("WINDSURF_API_KEY")
                .ok()
                .filter(|key| !key.is_empty()),
            model: std::env::var("WS_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string()),
            max_rounds: 3,
            max_results: 10,
            max_commands: 8,
            tree_depth: 3,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            exclude_paths: Vec::new(),
            include_code_snippets: false,
            repo_map_mode: "bootstrap_hotspot".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SearchEngine {
    config: SearchConfig,
    session_id: Uuid,
    client: reqwest::Client,
}

#[derive(Debug, Clone)]
struct ChatMessage {
    role: u64,
    content: String,
    tool_call_id: Option<String>,
    tool_name: Option<String>,
    tool_args_json: Option<String>,
    ref_call_id: Option<String>,
}

#[derive(Debug, Clone)]
struct RepoMap {
    tree: String,
    depth: usize,
    size_bytes: usize,
    fell_back: bool,
}

#[derive(Debug, Clone)]
pub struct ParsedResponse {
    pub text: String,
    pub tool_call: Option<ToolCall>,
}

impl SearchEngine {
    #[must_use]
    pub fn new(config: SearchConfig) -> Self {
        Self {
            config,
            session_id: Uuid::new_v4(),
            client: reqwest::Client::new(),
        }
    }

    #[must_use]
    pub fn metadata(&self) -> String {
        let key_state = if self.config.api_key.is_some() {
            "present"
        } else {
            "missing"
        };
        format!(
            "[config]\nproject_path={}\nmodel={}\nmax_rounds={}\nmax_results={}\ntree_depth={}\nsession_id={}\napi_key={}\nclient=rustls-reqwest",
            self.config.project_path.display(),
            self.config.model,
            self.config.max_rounds,
            self.config.max_results,
            self.config.tree_depth,
            self.session_id,
            key_state
        )
    }

    pub async fn search(&self, query: &str) -> Result<String, FastContextError> {
        let api_key = self.config.api_key.as_deref().ok_or_else(|| {
            FastContextError::new(
                FastContextErrorKind::MissingApiKey,
                "WINDSURF_API_KEY is required for real Windsurf search",
            )
        })?;

        let jwt = self.fetch_jwt(api_key).await?;
        if !self.check_rate_limit(api_key, &jwt).await? {
            return Err(FastContextError::new(
                FastContextErrorKind::RateLimited,
                "Windsurf CheckUserMessageRateLimit rejected the request",
            ));
        }

        let repo_map = build_repo_map(
            &self.config.project_path,
            self.config.tree_depth,
            &self.config.exclude_paths,
        );
        let executor = ToolExecutor::new(&self.config.project_path).map_err(|error| {
            FastContextError::new(
                FastContextErrorKind::InvalidResponse,
                format!("failed to create local restricted executor: {error}"),
            )
        })?;
        let tool_defs = build_tool_definitions(self.config.max_commands);
        let system_prompt = build_system_prompt(
            self.config.max_rounds,
            self.config.max_commands,
            self.config.max_results,
        );
        let user_content = format!(
            "Problem Statement: {query}\n\nRepo Map (tree -L {} /codebase):\n```text\n{}\n```",
            repo_map.depth, repo_map.tree
        );
        let mut messages = vec![
            ChatMessage::new(5, system_prompt),
            ChatMessage::new(1, user_content),
        ];
        let total_api_calls = self.config.max_rounds + 1;
        let mut force_answer_injected = false;

        for turn in 0..total_api_calls {
            let request = self.build_request(api_key, &jwt, &messages, &tool_defs);
            let response_bytes = self.streaming_request(&request).await?;
            let parsed = parse_streaming_response(&response_bytes)?;

            let Some(tool_call) = parsed.tool_call else {
                if parsed.text.starts_with("[Error]") {
                    return Err(FastContextError::new(
                        FastContextErrorKind::InvalidResponse,
                        parsed.text,
                    ));
                }
                return Ok(format_no_relevant_files(&parsed.text));
            };

            match tool_call.name.as_str() {
                "answer" => {
                    let answer_xml = tool_call
                        .args
                        .get("answer")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let ranges = parse_answer(answer_xml);
                    return Ok(self.format_search_result(&ranges, &executor, &repo_map));
                }
                "restricted_exec" => {
                    let call_id = Uuid::new_v4().to_string();
                    let args_json =
                        serde_json::to_string(&tool_call.args).unwrap_or_else(|_| "{}".to_string());
                    let results = executor.exec_tool_call(&tool_call.args);
                    messages.push(ChatMessage {
                        role: 2,
                        content: tool_call.thinking,
                        tool_call_id: Some(call_id.clone()),
                        tool_name: Some("restricted_exec".to_string()),
                        tool_args_json: Some(args_json),
                        ref_call_id: None,
                    });
                    messages.push(ChatMessage {
                        role: 4,
                        content: results,
                        tool_call_id: None,
                        tool_name: None,
                        tool_args_json: None,
                        ref_call_id: Some(call_id),
                    });

                    if turn >= self.config.max_rounds.saturating_sub(1) && !force_answer_injected {
                        messages.push(ChatMessage::new(1, FINAL_FORCE_ANSWER.to_string()));
                        force_answer_injected = true;
                    }
                }
                other => {
                    return Err(FastContextError::new(
                        FastContextErrorKind::InvalidResponse,
                        format!("unexpected Windsurf tool call: {other}"),
                    ));
                }
            }
        }

        let parts = [
            "No relevant files found.".to_string(),
            String::new(),
            "[diagnostic] max turns reached without getting an answer".to_string(),
            format_config_line(&self.config, &repo_map, 0),
        ];
        Ok(parts.join("\n"))
    }

    async fn fetch_jwt(&self, api_key: &str) -> Result<String, FastContextError> {
        let meta = ProtobufEncoder::new()
            .write_string(1, WS_APP)
            .write_string(2, &ws_app_version())
            .write_string(3, api_key)
            .write_string(4, "zh-cn")
            .write_string(7, &ws_ls_version())
            .write_string(12, WS_APP)
            .write_bytes(30, &[0x00, 0x01]);
        let request = ProtobufEncoder::new().write_message(1, &meta).to_vec();
        let response = self
            .unary_request(&format!("{AUTH_BASE}/GetUserJwt"), &request, false)
            .await?;
        for text in extract_strings(&response) {
            if text.starts_with("eyJ") && text.contains('.') {
                return Ok(text);
            }
        }
        Err(FastContextError::new(
            FastContextErrorKind::AuthError,
            "failed to extract JWT from GetUserJwt response",
        ))
    }

    async fn check_rate_limit(&self, api_key: &str, jwt: &str) -> Result<bool, FastContextError> {
        let request = ProtobufEncoder::new()
            .write_message(1, &self.build_metadata(api_key, jwt))
            .write_string(3, &self.config.model)
            .to_vec();
        match self
            .unary_request(
                &format!("{API_BASE}/CheckUserMessageRateLimit"),
                &request,
                true,
            )
            .await
        {
            Ok(_) => Ok(true),
            Err(error) if error.kind == FastContextErrorKind::RateLimited => Ok(false),
            Err(_) => Ok(true),
        }
    }

    async fn unary_request(
        &self,
        url: &str,
        proto_bytes: &[u8],
        compress: bool,
    ) -> Result<Vec<u8>, FastContextError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Content-Type",
            HeaderValue::from_static("application/proto"),
        );
        headers.insert("Connect-Protocol-Version", HeaderValue::from_static("1"));
        headers.insert(
            "User-Agent",
            HeaderValue::from_static("connect-go/1.18.1 (go1.25.5)"),
        );
        headers.insert("Accept-Encoding", HeaderValue::from_static("gzip"));

        let body = if compress {
            headers.insert("Content-Encoding", HeaderValue::from_static("gzip"));
            gzip_bytes(proto_bytes)?
        } else {
            proto_bytes.to_vec()
        };

        let response = self
            .client
            .post(url)
            .headers(headers)
            .body(body)
            .timeout(Duration::from_millis(self.config.timeout_ms))
            .send()
            .await
            .map_err(classify_reqwest_error)?;
        response_bytes(response).await
    }

    async fn streaming_request(&self, proto_bytes: &[u8]) -> Result<Vec<u8>, FastContextError> {
        let frame = connect_frame_encode(proto_bytes, true)?;
        let trace_id = self.session_id.simple().to_string();
        let span_id = Uuid::new_v4().simple().to_string()[..16].to_string();
        let timeout_ms = self.config.timeout_ms;
        let abort_ms = timeout_ms.saturating_add(5_000);

        let mut headers = HeaderMap::new();
        headers.insert(
            "Content-Type",
            HeaderValue::from_static("application/connect+proto"),
        );
        headers.insert("Connect-Protocol-Version", HeaderValue::from_static("1"));
        headers.insert("Connect-Accept-Encoding", HeaderValue::from_static("gzip"));
        headers.insert("Connect-Content-Encoding", HeaderValue::from_static("gzip"));
        headers.insert(
            "Connect-Timeout-Ms",
            HeaderValue::from_str(&timeout_ms.to_string()).map_err(|error| {
                FastContextError::new(
                    FastContextErrorKind::InvalidResponse,
                    format!("invalid timeout header: {error}"),
                )
            })?,
        );
        headers.insert(
            "User-Agent",
            HeaderValue::from_static("connect-go/1.18.1 (go1.25.5)"),
        );
        headers.insert("Accept-Encoding", HeaderValue::from_static("identity"));
        headers.insert(
            "Baggage",
            HeaderValue::from_str(&format!(
                "sentry-release=language-server-windsurf@{},sentry-environment=stable,sentry-sampled=false,sentry-trace_id={},sentry-public_key=b813f73488da69eedec534dba1029111",
                ws_ls_version(), trace_id
            ))
            .map_err(|error| {
                FastContextError::new(
                    FastContextErrorKind::InvalidResponse,
                    format!("invalid baggage header: {error}"),
                )
            })?,
        );
        headers.insert(
            "Sentry-Trace",
            HeaderValue::from_str(&format!("{trace_id}-{span_id}-0")).map_err(|error| {
                FastContextError::new(
                    FastContextErrorKind::InvalidResponse,
                    format!("invalid sentry trace header: {error}"),
                )
            })?,
        );

        let response = self
            .client
            .post(format!("{API_BASE}/GetDevstralStream"))
            .headers(headers)
            .body(frame)
            .timeout(Duration::from_millis(abort_ms))
            .send()
            .await
            .map_err(classify_reqwest_error)?;
        response_bytes(response).await
    }

    fn build_metadata(&self, api_key: &str, jwt: &str) -> ProtobufEncoder {
        let sys_info = json!({
            "Os": std::env::consts::OS,
            "Arch": std::env::consts::ARCH,
            "Release": "",
            "Version": "",
            "Machine": std::env::consts::ARCH,
            "Nodename": "",
            "Sysname": if cfg!(target_os = "windows") { "Windows_NT" } else if cfg!(target_os = "macos") { "Darwin" } else { "Linux" },
            "ProductVersion": ""
        });
        let cpu_info = json!({
            "NumSockets": 1,
            "NumCores": 4,
            "NumThreads": 4,
            "VendorID": "",
            "Family": "0",
            "Model": "0",
            "ModelName": "Unknown",
            "Memory": 0
        });
        ProtobufEncoder::new()
            .write_string(1, WS_APP)
            .write_string(2, &ws_app_version())
            .write_string(3, api_key)
            .write_string(4, "zh-cn")
            .write_string(5, &sys_info.to_string())
            .write_string(7, &ws_ls_version())
            .write_string(8, &cpu_info.to_string())
            .write_string(12, WS_APP)
            .write_string(21, jwt)
            .write_bytes(30, &[0x00, 0x01])
    }

    fn build_request(
        &self,
        api_key: &str,
        jwt: &str,
        messages: &[ChatMessage],
        tool_defs: &str,
    ) -> Vec<u8> {
        let mut request =
            ProtobufEncoder::new().write_message(1, &self.build_metadata(api_key, jwt));
        for message in messages {
            request = request.write_message(2, &build_chat_message(message));
        }
        request.write_string(3, tool_defs).to_vec()
    }

    fn format_search_result(
        &self,
        ranges: &[AnswerFileRange],
        executor: &ToolExecutor,
        repo_map: &RepoMap,
    ) -> String {
        let mut files = ranges
            .iter()
            .take(self.config.max_results)
            .map(|range| ResultFile {
                path: range.path.clone(),
                ranges: vec![(range.start, range.end)],
                from_grep: false,
            })
            .collect::<Vec<_>>();

        let mut patterns = executor.collected_rg_patterns();
        patterns.retain(|pattern| pattern.chars().count() >= 3);
        patterns.sort();
        patterns.dedup();

        let grep_budget = self.config.max_results.saturating_sub(files.len());
        let grep_expanded = if grep_budget > 0 && !patterns.is_empty() {
            let mut exclude_paths = merged_exclude_paths(&self.config.exclude_paths);
            exclude_paths.extend(grep_noise_globs());
            exclude_paths.sort();
            exclude_paths.dedup();
            auto_grep_files(executor, &patterns, &exclude_paths, &mut files, grep_budget)
        } else {
            0
        };

        if files.is_empty() {
            return format!(
                "No relevant files found.\n\n{}",
                format_config_line(&self.config, repo_map, grep_expanded)
            );
        }

        let summary = if grep_expanded > 0 {
            format!(
                "Found {} relevant files ({} from AI search, {} from grep keyword expansion).",
                files.len(),
                files.len().saturating_sub(grep_expanded),
                grep_expanded
            )
        } else {
            format!("Found {} relevant files.", files.len())
        };
        let mut parts = vec![summary];

        for (index, file) in files.iter().enumerate() {
            let full_path = self.config.project_path.join(&file.path);
            let ranges_label = if file.ranges.is_empty() {
                if file.from_grep {
                    "grep match".to_string()
                } else {
                    String::new()
                }
            } else {
                file.ranges
                    .iter()
                    .map(|(start, end)| format!("L{start}-{end}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let grep_label = if file.from_grep {
                " [grep expanded]"
            } else {
                ""
            };
            parts.push(String::new());
            parts.push(format!(
                "--- [{}/{}] {} ({}){} ---",
                index + 1,
                files.len(),
                full_path.display(),
                ranges_label,
                grep_label
            ));
            if self.config.include_code_snippets {
                if file.ranges.is_empty() {
                    parts.push(executor.readfile(
                        &format!("/codebase/{}", file.path),
                        Some(1),
                        Some(20),
                    ));
                } else {
                    for (start, end) in &file.ranges {
                        parts.push(executor.readfile(
                            &format!("/codebase/{}", file.path),
                            Some(*start),
                            Some(*end),
                        ));
                    }
                }
            }
        }

        if !patterns.is_empty() {
            parts.push(String::new());
            parts.push(format!("grep keywords: {}", patterns.join(", ")));
        }
        parts.push(String::new());
        parts.push(format_config_line(&self.config, repo_map, grep_expanded));
        parts.join("\n")
    }
}
#[derive(Debug, Clone)]
struct ResultFile {
    path: String,
    ranges: Vec<(usize, usize)>,
    from_grep: bool,
}

fn auto_grep_files(
    executor: &ToolExecutor,
    patterns: &[String],
    exclude_paths: &[String],
    files: &mut Vec<ResultFile>,
    max_total: usize,
) -> usize {
    let mut added = 0;
    for pattern in patterns.iter().take(8) {
        if added >= max_total {
            break;
        }
        for path in executor.rg_files(
            pattern,
            Some(exclude_paths),
            3.min(max_total.saturating_sub(added)),
        ) {
            if files.iter().any(|file| file.path == path) {
                continue;
            }
            files.push(ResultFile {
                path,
                ranges: Vec::new(),
                from_grep: true,
            });
            added += 1;
            if added >= max_total {
                break;
            }
        }
    }
    added
}

fn format_config_line(config: &SearchConfig, repo_map: &RepoMap, grep_expanded: usize) -> String {
    let mut line = format!(
        "[config] project_path={}, tree_depth={}{} , tree_size={}KB, max_turns={}, max_results={}, timeout_ms={}",
        config.project_path.display(),
        repo_map.depth,
        if repo_map.fell_back {
            " (fell back from requested depth)"
        } else {
            ""
        },
        format_kb(repo_map.size_bytes),
        config.max_rounds,
        config.max_results,
        config.timeout_ms
    );
    if !config.exclude_paths.is_empty() {
        line.push_str(&format!(
            ", exclude_paths=[{}]",
            config.exclude_paths.join(", ")
        ));
    }
    if grep_expanded > 0 {
        line.push_str(&format!(", grep_expanded={grep_expanded}"));
    }
    line
}

fn format_kb(bytes: usize) -> String {
    let kb = bytes as f64 / 1024.0;
    if kb >= 10.0 || (kb.fract()).abs() < f64::EPSILON {
        format!("{}", kb.round() as usize)
    } else {
        format!("{kb:.1}")
    }
}

fn merged_exclude_paths(user_exclude_paths: &[String]) -> Vec<String> {
    let mut excludes = DEFAULT_EXCLUDE_PATHS
        .iter()
        .map(|value| (*value).to_string())
        .collect::<Vec<_>>();
    for path in user_exclude_paths {
        if !path.is_empty() && !excludes.contains(path) {
            excludes.push(path.clone());
        }
    }
    excludes
}

fn grep_noise_globs() -> Vec<String> {
    [
        "chunk-*",
        "*.chunk.*",
        "*.bundle.*",
        "*.min.js",
        "*.min.css",
        "*.map",
        "app.*.js",
        "app.*.css",
        "*.lock",
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "go.sum",
        "go.mod",
        "Cargo.lock",
        "*.d.ts",
        "*.exe",
        "*.dll",
        "*.so",
        "*.dylib",
        "*.pyc",
        "*.pyo",
        "*.class",
        "*.wasm",
        "*.o",
        "*.a",
        "*.svg",
        "*.png",
        "*.jpg",
        "*.jpeg",
        "*.gif",
        "*.ico",
        "*.webp",
        "*.woff*",
        "*.ttf",
        "*.eot",
        "*.otf",
        "*.mp4",
        "*.mp3",
        "*.wav",
        "*.avi",
        "*.mov",
        "*.pdf",
        "*.doc",
        "*.docx",
        "*.xls",
        "*.xlsx",
        "*.zip",
        "*.tar",
        "*.gz",
        "*.rar",
        "*.7z",
        "CLAUDE.md",
        "AGENTS.md",
        ".cursorrules",
        ".cursorignore",
        "dist/**",
        "build/**",
        "out/**",
        "target/**",
        "resource/page/**",
        ".nuxt/**",
        ".next/**",
        ".output/**",
        "__pycache__/**",
        ".cache/**",
        ".pytest_cache/**",
        ".svn/**",
        ".hg/**",
        "vendor/**",
        ".venv/**",
        "venv/**",
    ]
    .iter()
    .map(|value| (*value).to_string())
    .collect()
}

impl ChatMessage {
    fn new(role: u64, content: String) -> Self {
        Self {
            role,
            content,
            tool_call_id: None,
            tool_name: None,
            tool_args_json: None,
            ref_call_id: None,
        }
    }
}

fn build_chat_message(message: &ChatMessage) -> ProtobufEncoder {
    let mut encoded = ProtobufEncoder::new()
        .write_varint(2, message.role)
        .write_string(3, &message.content);
    if let (Some(call_id), Some(tool_name), Some(args_json)) = (
        &message.tool_call_id,
        &message.tool_name,
        &message.tool_args_json,
    ) {
        let tool_call = ProtobufEncoder::new()
            .write_string(1, call_id)
            .write_string(2, tool_name)
            .write_string(3, args_json);
        encoded = encoded.write_message(6, &tool_call);
    }
    if let Some(ref_call_id) = &message.ref_call_id {
        encoded = encoded.write_string(7, ref_call_id);
    }
    encoded
}

fn ws_app_version() -> String {
    std::env::var("WS_APP_VER").unwrap_or_else(|_| DEFAULT_WS_APP_VER.to_string())
}

fn ws_ls_version() -> String {
    std::env::var("WS_LS_VER").unwrap_or_else(|_| DEFAULT_WS_LS_VER.to_string())
}

fn build_system_prompt(max_turns: usize, max_commands: usize, max_results: usize) -> String {
    SYSTEM_PROMPT_TEMPLATE
        .replace("{max_turns}", &max_turns.to_string())
        .replace("{max_commands}", &max_commands.to_string())
        .replace("{max_results}", &max_results.to_string())
}

fn build_tool_definitions(max_commands: usize) -> String {
    let mut properties = serde_json::Map::new();
    for index in 1..=max_commands.max(1) {
        properties.insert(
            format!("command{index}"),
            json!({
                "type": "object",
                "description": format!("Command {index} to execute. Must be one of: rg, readfile, tree, ls, glob."),
                "oneOf": [
                    {
                        "properties": {
                            "type": { "type": "string", "const": "rg" },
                            "pattern": { "type": "string" },
                            "path": { "type": "string" },
                            "include": { "type": "array", "items": { "type": "string" } },
                            "exclude": { "type": "array", "items": { "type": "string" } }
                        },
                        "required": ["type", "pattern", "path"]
                    },
                    {
                        "properties": {
                            "type": { "type": "string", "const": "readfile" },
                            "file": { "type": "string" },
                            "start_line": { "type": "integer" },
                            "end_line": { "type": "integer" }
                        },
                        "required": ["type", "file"]
                    },
                    {
                        "properties": {
                            "type": { "type": "string", "const": "tree" },
                            "path": { "type": "string" },
                            "levels": { "type": "integer" }
                        },
                        "required": ["type", "path"]
                    },
                    {
                        "properties": {
                            "type": { "type": "string", "const": "ls" },
                            "path": { "type": "string" },
                            "long_format": { "type": "boolean" },
                            "all": { "type": "boolean" }
                        },
                        "required": ["type", "path"]
                    },
                    {
                        "properties": {
                            "type": { "type": "string", "const": "glob" },
                            "pattern": { "type": "string" },
                            "path": { "type": "string" },
                            "type_filter": { "type": "string", "enum": ["file", "directory", "all"] }
                        },
                        "required": ["type", "pattern", "path"]
                    }
                ]
            }),
        );
    }

    json!([
        {
            "type": "function",
            "function": {
                "name": "restricted_exec",
                "description": "Execute restricted commands (rg, readfile, tree, ls, glob) in parallel.",
                "parameters": { "type": "object", "properties": properties, "required": ["command1"] }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "answer",
                "description": "Final answer with relevant files and line ranges.",
                "parameters": {
                    "type": "object",
                    "properties": { "answer": { "type": "string", "description": "The final answer in XML format." } },
                    "required": ["answer"]
                }
            }
        }
    ])
    .to_string()
}

fn build_repo_map(project_root: &Path, target_depth: usize, exclude_paths: &[String]) -> RepoMap {
    let mut excludes = DEFAULT_EXCLUDE_PATHS
        .iter()
        .map(|value| (*value).to_string())
        .collect::<Vec<_>>();
    for path in exclude_paths {
        if !path.is_empty() && !excludes.contains(path) {
            excludes.push(path.clone());
        }
    }

    let requested_depth = if target_depth == 0 {
        suggest_tree_depth(project_root)
    } else {
        target_depth.clamp(1, 6)
    };

    for depth in (1..=requested_depth).rev() {
        let tree = render_tree(project_root, depth, &excludes);
        let size_bytes = tree.len();
        if size_bytes <= MAX_TREE_BYTES {
            return RepoMap {
                tree,
                depth,
                size_bytes,
                fell_back: depth < requested_depth,
            };
        }
    }

    let tree = render_tree(project_root, 1, &excludes);
    RepoMap {
        size_bytes: tree.len(),
        tree,
        depth: 1,
        fell_back: true,
    }
}

fn suggest_tree_depth(project_root: &Path) -> usize {
    let count = fs::read_dir(project_root)
        .map(|dir| dir.count())
        .unwrap_or(0);
    if count < 500 {
        4
    } else if count <= 5000 {
        3
    } else {
        2
    }
}

fn render_tree(project_root: &Path, depth: usize, excludes: &[String]) -> String {
    let mut lines = vec!["/codebase".to_string()];
    append_repo_tree(project_root, "", depth, excludes, &mut lines);
    lines.join("\n")
}

fn append_repo_tree(
    dir: &Path,
    prefix: &str,
    depth_remaining: usize,
    excludes: &[String],
    lines: &mut Vec<String>,
) {
    if depth_remaining == 0 {
        return;
    }
    let mut entries = match fs::read_dir(dir) {
        Ok(entries) => entries.filter_map(Result::ok).collect::<Vec<_>>(),
        Err(_) => return,
    };
    entries.sort_by_key(|entry| entry.file_name());
    entries.retain(|entry| !is_excluded_name(&entry.file_name().to_string_lossy(), excludes));

    let count = entries.len();
    for (index, entry) in entries.into_iter().enumerate() {
        let is_last = index + 1 == count;
        let connector = if is_last { "└── " } else { "├── " };
        let name = entry.file_name().to_string_lossy().into_owned();
        lines.push(format!("{prefix}{connector}{name}"));
        if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            let next_prefix = format!("{prefix}{}", if is_last { "    " } else { "│   " });
            append_repo_tree(
                &entry.path(),
                &next_prefix,
                depth_remaining - 1,
                excludes,
                lines,
            );
        }
    }
}

fn is_excluded_name(name: &str, excludes: &[String]) -> bool {
    excludes.iter().any(|pattern| {
        if let Some(stripped) = pattern.strip_prefix("**/") {
            fnmatch_simple(name, stripped)
        } else {
            fnmatch_simple(name, pattern) || name == pattern
        }
    })
}

fn fnmatch_simple(value: &str, pattern: &str) -> bool {
    if !pattern.contains('*') && !pattern.contains('?') {
        return value == pattern;
    }
    let mut regex = String::from("^");
    for ch in pattern.chars() {
        match ch {
            '*' => regex.push_str(".*"),
            '?' => regex.push('.'),
            '.' | '+' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\' => {
                regex.push('\\');
                regex.push(ch);
            }
            other => regex.push(other),
        }
    }
    regex.push('$');
    Regex::new(&regex)
        .map(|regex| regex.is_match(value))
        .unwrap_or(false)
}

pub fn parse_streaming_response(data: &[u8]) -> Result<ParsedResponse, FastContextError> {
    let frames = connect_frame_decode(data)?;
    let mut all_text = String::new();
    let mut found_tool_calls = false;

    for frame in frames {
        let raw_text = String::from_utf8_lossy(&frame).replace('\u{fffd}', "");
        let trimmed = raw_text.trim_start();
        if trimmed.starts_with('{')
            && let Ok(value) = serde_json::from_str::<Value>(trimmed)
            && let Some(error) = value.get("error")
        {
            let code = error
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            return Ok(ParsedResponse {
                text: format!("[Error] {code}: {message}"),
                tool_call: None,
            });
        }

        let extracted_strings = extract_strings(&frame);
        let extracted_text = if extracted_strings.is_empty() {
            None
        } else {
            Some(extracted_strings.join(""))
        };
        let tool_text = if found_tool_calls {
            extracted_text.as_deref().unwrap_or(&raw_text)
        } else {
            extracted_text
                .as_deref()
                .filter(|text| text.contains("[TOOL_CALLS]"))
                .unwrap_or(&raw_text)
        };

        // Once we've seen [TOOL_CALLS], keep concatenating subsequent text and try to
        // parse incrementally. Prefer protobuf-extracted strings over raw frame bytes:
        // a single Connect frame can contain several length-delimited protobuf string
        // chunks, and raw UTF-8 decoding leaves tag/length control bytes between those
        // chunks. Those bytes can land inside the JSON args and make a valid tool call
        // look malformed, causing the user-visible "Raw response" fallback.
        if found_tool_calls {
            all_text.push_str(tool_text);
            if parse_tool_call(&all_text).is_some() {
                break;
            }
            continue;
        }

        if tool_text.contains("[TOOL_CALLS]") {
            all_text = tool_text.to_string();
            found_tool_calls = true;
            if parse_tool_call(&all_text).is_some() {
                break;
            }
            continue;
        }

        if let Some(extracted_text) = extracted_text {
            for text in extracted_text.split('\0') {
                if text.len() > 10 {
                    all_text.push_str(text);
                }
            }
        }
    }

    let tool_call = parse_tool_call(&all_text);
    Ok(ParsedResponse {
        text: all_text,
        tool_call,
    })
}

fn format_no_relevant_files(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        "No relevant files found.".to_string()
    } else {
        let truncated = if raw.chars().count() > 500 {
            format!(
                "{}\n...[raw_response truncated]...",
                raw.chars().take(500).collect::<String>()
            )
        } else {
            raw.to_string()
        };
        format!("No relevant files found.\n\nRaw response:\n{truncated}")
    }
}

fn gzip_bytes(bytes: &[u8]) -> Result<Vec<u8>, FastContextError> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).map_err(|error| {
        FastContextError::new(
            FastContextErrorKind::InvalidResponse,
            format!("gzip encode failed: {error}"),
        )
    })?;
    encoder.finish().map_err(|error| {
        FastContextError::new(
            FastContextErrorKind::InvalidResponse,
            format!("gzip finish failed: {error}"),
        )
    })
}

async fn response_bytes(response: reqwest::Response) -> Result<Vec<u8>, FastContextError> {
    let status = response.status();
    if !status.is_success() {
        return Err(classify_http_status(
            status.as_u16(),
            format!("HTTP {status}"),
        ));
    }
    let is_gzip = response
        .headers()
        .get(header::CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.eq_ignore_ascii_case("gzip"))
        .unwrap_or(false);
    let bytes = response
        .bytes()
        .await
        .map_err(classify_reqwest_error)?
        .to_vec();
    if is_gzip {
        let mut decoder = GzDecoder::new(bytes.as_slice());
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).map_err(|error| {
            FastContextError::new(
                FastContextErrorKind::InvalidResponse,
                format!("gzip decode failed: {error}"),
            )
        })?;
        Ok(out)
    } else {
        Ok(bytes)
    }
}

fn classify_reqwest_error(error: reqwest::Error) -> FastContextError {
    if error.is_timeout() {
        return FastContextError::new(FastContextErrorKind::Timeout, error.to_string());
    }
    if let Some(status) = error.status() {
        return classify_http_status(status.as_u16(), error.to_string());
    }
    FastContextError::new(FastContextErrorKind::NetworkError, error.to_string())
}

#[must_use]
pub fn jwt_exp_unix(jwt: &str) -> Option<u64> {
    let payload = jwt.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let value = serde_json::from_slice::<Value>(&decoded).ok()?;
    value.get("exp").and_then(Value::as_u64)
}

pub fn parse_tool_call(text: &str) -> Option<ToolCall> {
    let marker = "[TOOL_CALLS]";
    let args_marker = "[ARGS]";
    let cleaned = text.replace("</s>", "");
    let marker_index = cleaned.find(marker)?;
    let thinking = cleaned[..marker_index].trim().to_string();
    let after_marker = cleaned[marker_index + marker.len()..].trim_start();

    let (name, args_text) = if let Some(args_index) = after_marker.find(args_marker) {
        (
            after_marker[..args_index].trim(),
            after_marker[args_index + args_marker.len()..].trim(),
        )
    } else {
        let json_start = after_marker.find('{')?;
        (
            after_marker[..json_start].trim(),
            after_marker[json_start..].trim(),
        )
    };

    if name.is_empty()
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return None;
    }

    let json_candidate = json_object_candidate(args_text)?;
    let mut args = parse_json_lenient(&json_candidate)?;
    if name == "restricted_exec" {
        normalize_restricted_exec_args(&mut args);
    }
    Some(ToolCall {
        thinking,
        name: name.to_string(),
        args,
    })
}

fn json_object_candidate(input: &str) -> Option<String> {
    extract_json_object(input)
        .map(ToString::to_string)
        .or_else(|| repair_unclosed_json_object(input))
}

fn extract_json_object(input: &str) -> Option<&str> {
    let start = input.find('{')?;
    let mut depth = 0_u32;
    let mut in_string = false;
    let mut escape = false;

    for (offset, ch) in input[start..].char_indices() {
        if in_string {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    let end = start + offset + ch.len_utf8();
                    return Some(&input[start..end]);
                }
            }
            _ => {}
        }
    }

    None
}

fn repair_unclosed_json_object(input: &str) -> Option<String> {
    let start = input.find('{')?;
    let mut out = String::from(&input[start..]);
    let mut stack = Vec::new();
    let mut in_string = false;
    let mut escape = false;

    for ch in input[start..].chars() {
        if in_string {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' if stack.pop() != Some(ch) => {
                return None;
            }
            _ => {}
        }
    }

    if in_string || stack.is_empty() {
        return None;
    }

    while let Some(ch) = stack.pop() {
        out.push(ch);
    }
    Some(out)
}

fn normalize_restricted_exec_args(args: &mut Value) {
    let Some(commands) = args.as_object_mut() else {
        return;
    };

    let mut nested_commands = Vec::new();
    for command in commands.values() {
        let Some(object) = command.as_object() else {
            continue;
        };
        for (key, value) in object {
            if is_command_key(key) && value.is_object() && !commands.contains_key(key) {
                nested_commands.push((key.clone(), value.clone()));
            }
        }
    }
    for (key, value) in nested_commands {
        commands.entry(key).or_insert(value);
    }

    for command in commands.values_mut() {
        let Some(object) = command.as_object_mut() else {
            continue;
        };
        if object.get("type").and_then(Value::as_str).is_some() {
            continue;
        }

        for tool_name in ["rg", "readfile", "tree", "ls", "glob"] {
            let Some(shorthand_value) = object.remove(tool_name) else {
                continue;
            };

            object.insert("type".to_string(), Value::String(tool_name.to_string()));
            if let Some(shorthand_object) = shorthand_value.as_object() {
                for (key, value) in shorthand_object {
                    object.entry(key.clone()).or_insert_with(|| value.clone());
                }
            } else if let Some(target_key) = shorthand_target_key(tool_name) {
                object
                    .entry(target_key.to_string())
                    .or_insert(shorthand_value);
            }
            break;
        }
    }
}

fn is_command_key(key: &str) -> bool {
    let Some(number) = key.strip_prefix("command") else {
        return false;
    };
    !number.is_empty() && number.chars().all(|ch| ch.is_ascii_digit())
}

fn shorthand_target_key(tool_name: &str) -> Option<&'static str> {
    match tool_name {
        "rg" | "glob" => Some("pattern"),
        "readfile" => Some("file"),
        "tree" | "ls" => Some("path"),
        _ => None,
    }
}

pub fn parse_answer(text: &str) -> Vec<AnswerFileRange> {
    let Ok(file_re) = Regex::new(r#"(?s)<file\b([^>]*)/\s*>|<file\b([^>]*)>(.*?)</file\s*>"#)
    else {
        return Vec::new();
    };
    let mut ranges = Vec::new();

    for capture in file_re.captures_iter(text) {
        let attrs = capture
            .get(1)
            .or_else(|| capture.get(2))
            .map(|matched| matched.as_str())
            .unwrap_or_default();
        let body = capture
            .get(3)
            .map(|matched| matched.as_str())
            .unwrap_or_default();
        let Some(path) = attr(attrs, "path").and_then(normalize_answer_path) else {
            continue;
        };

        if let (Some(start), Some(end)) = (attr(attrs, "start"), attr(attrs, "end")) {
            if let Some((start, end)) = parse_range_pair(&start, &end) {
                ranges.push(AnswerFileRange {
                    path: path.clone(),
                    start,
                    end,
                });
            }
            continue;
        }

        let mut found_body_range = false;
        for (start, end) in parse_body_ranges(body) {
            found_body_range = true;
            ranges.push(AnswerFileRange {
                path: path.clone(),
                start,
                end,
            });
        }
        if !found_body_range {
            ranges.push(AnswerFileRange {
                path,
                start: 1,
                end: 1,
            });
        }
    }

    ranges
}

pub fn parse_connect_response(bytes: &[u8]) -> Result<Vec<String>, FastContextError> {
    let frames = connect_frame_decode(bytes)?;
    let mut strings = Vec::new();

    for frame in frames {
        let extracted = extract_strings(&frame);
        if !extracted.is_empty() {
            strings.extend(extracted);
            continue;
        }

        if let Ok(text) = std::str::from_utf8(&frame)
            && !text.trim().is_empty()
        {
            strings.push(text.to_string());
        }
    }

    Ok(strings)
}

fn parse_json_lenient(input: &str) -> Option<Value> {
    let without_trailing_commas = remove_trailing_json_commas(input);
    let quoted = quote_bare_json(input);
    let quoted_without_trailing_commas = remove_trailing_json_commas(&quoted);

    serde_json::from_str(input)
        .or_else(|_| serde_json::from_str(&without_trailing_commas))
        .or_else(|_| serde_json::from_str(&quoted))
        .or_else(|_| serde_json::from_str(&quoted_without_trailing_commas))
        .ok()
}

fn remove_trailing_json_commas(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escape = false;

    while let Some(ch) = chars.next() {
        if in_string {
            out.push(ch);
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => {
                in_string = true;
                out.push(ch);
            }
            ',' => {
                if chars
                    .clone()
                    .find(|next| !next.is_whitespace())
                    .is_some_and(|next| next == '}' || next == ']')
                {
                    continue;
                }
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }

    out
}

fn quote_bare_json(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 16);
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escape = false;
    let mut expecting_key = true;

    while let Some(ch) = chars.next() {
        if in_string {
            out.push(ch);
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => {
                in_string = true;
                out.push(ch);
            }
            '{' | ',' => {
                expecting_key = true;
                out.push(ch);
            }
            ':' => {
                expecting_key = false;
                out.push(ch);
            }
            '}' | ']' => {
                expecting_key = false;
                out.push(ch);
            }
            '[' => {
                expecting_key = false;
                out.push(ch);
            }
            c if (expecting_key || value_position_needs_quote(&out)) && is_ident_start(c) => {
                let mut ident = String::from(c);
                while let Some(next) = chars.peek().copied() {
                    if is_ident_continue(next) {
                        ident.push(next);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let is_value = value_position_needs_quote(&out);
                if is_value && matches!(ident.as_str(), "true" | "false" | "null") {
                    out.push_str(&ident);
                } else {
                    out.push('"');
                    out.push_str(&ident);
                    out.push('"');
                }
                if expecting_key {
                    expecting_key = false;
                }
            }
            _ => out.push(ch),
        }
    }

    out
}

fn value_position_needs_quote(out: &str) -> bool {
    let mut chars = out.chars().rev().skip_while(|ch| ch.is_whitespace());
    matches!(chars.next(), Some(':'))
}

fn is_ident_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_'
}

fn is_ident_continue(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.' || ch == '/'
}

fn attr(attrs: &str, name: &str) -> Option<String> {
    let pattern = format!(r#"{}\s*=\s*['\"]([^'\"]+)['\"]"#, regex::escape(name));
    Regex::new(&pattern)
        .ok()?
        .captures(attrs)?
        .get(1)
        .map(|matched| html_unescape(matched.as_str()))
}

fn html_unescape(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn normalize_answer_path(path: String) -> Option<String> {
    let normalized = path.trim().replace('\\', "/");
    let relative = normalized
        .strip_prefix("/codebase/")
        .or_else(|| normalized.strip_prefix("/codebase"))
        .unwrap_or(&normalized)
        .trim_start_matches('/');
    if relative.is_empty() {
        return None;
    }

    let mut parts = Vec::new();
    for component in PathBuf::from(relative).components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

fn parse_range_pair(start: &str, end: &str) -> Option<(usize, usize)> {
    let start = start.trim().parse::<usize>().ok()?;
    let end = end.trim().parse::<usize>().ok()?;
    if start == 0 || end == 0 || start > end {
        None
    } else {
        Some((start, end))
    }
}

fn parse_body_ranges(body: &str) -> Vec<(usize, usize)> {
    let Ok(range_re) = Regex::new(r#"<range[^>]*>\s*(\d+)\s*[-:]\s*(\d+)\s*</range>"#) else {
        return Vec::new();
    };
    range_re
        .captures_iter(body)
        .filter_map(|capture| parse_range_pair(&capture[1], &capture[2]))
        .collect()
}

#[must_use]
pub fn command_map(args: &Value) -> BTreeMap<String, Value> {
    args.as_object()
        .map(|object| {
            object
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default()
}
