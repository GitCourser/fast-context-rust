use std::cell::RefCell;
use std::cmp::Ordering;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;
use walkdir::{DirEntry, WalkDir};

use crate::config::{ExecutorLimits, RipgrepError, resolve_ripgrep_path};

#[derive(Debug, Error)]
pub enum ExecutorCreateError {
    #[error("failed to validate project root: {0}")]
    Root(io::Error),
    #[error(transparent)]
    Ripgrep(#[from] RipgrepError),
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolCommand {
    #[serde(rename = "type", default)]
    pub command_type: String,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub include: Option<Vec<String>>,
    #[serde(default)]
    pub exclude: Option<Vec<String>>,
    #[serde(default)]
    pub start_line: Option<usize>,
    #[serde(default)]
    pub end_line: Option<usize>,
    #[serde(default)]
    pub levels: Option<usize>,
    #[serde(default)]
    pub long_format: bool,
    #[serde(default, rename = "all")]
    pub all_files: bool,
    #[serde(default)]
    pub type_filter: Option<String>,
}

#[derive(Debug)]
pub struct ToolExecutor {
    root: PathBuf,
    rg_path: PathBuf,
    limits: ExecutorLimits,
    collected_rg_patterns: RefCell<Vec<String>>,
}

impl ToolExecutor {
    pub fn new(project_root: impl AsRef<Path>) -> Result<Self, ExecutorCreateError> {
        let rg_path = resolve_ripgrep_path()?;
        Self::with_rg_path(project_root, rg_path)
    }

    pub fn with_rg_path(
        project_root: impl AsRef<Path>,
        rg_path: impl Into<PathBuf>,
    ) -> Result<Self, ExecutorCreateError> {
        Self::with_rg_path_and_limits(project_root, rg_path, ExecutorLimits::default())
    }

    pub fn with_rg_path_and_limits(
        project_root: impl AsRef<Path>,
        rg_path: impl Into<PathBuf>,
        limits: ExecutorLimits,
    ) -> Result<Self, ExecutorCreateError> {
        let root = fs::canonicalize(project_root).map_err(ExecutorCreateError::Root)?;
        Ok(Self {
            root,
            rg_path: rg_path.into(),
            limits,
            collected_rg_patterns: RefCell::new(Vec::new()),
        })
    }

    pub fn collected_rg_patterns(&self) -> Vec<String> {
        self.collected_rg_patterns.borrow().clone()
    }

    fn real(&self, virtual_path: &str) -> Option<PathBuf> {
        let normalized = virtual_path.trim().replace('\\', "/");
        if normalized != "/codebase" && !normalized.starts_with("/codebase/") {
            return None;
        }

        let rel = normalized
            .trim_start_matches("/codebase")
            .trim_start_matches('/');
        let clean_rel = clean_relative_path(rel)?;
        let abs = self.root.join(clean_rel);
        if !is_within_lexically(&abs, &self.root) {
            return None;
        }

        if let Ok(canonical) = fs::canonicalize(&abs) {
            if !canonical.starts_with(&self.root) {
                return None;
            }
            return Some(canonical);
        }

        Some(abs)
    }

    pub fn path_error(kind: &str, value: &str) -> String {
        format!("Error: {kind} must stay within /codebase: {value}")
    }

    pub fn truncate(&self, text: &str) -> String {
        truncate_with_limits(text, &self.limits)
    }

    pub fn remap(&self, text: &str) -> String {
        let root = self.root.to_string_lossy();
        let mut result = text.replace(root.as_ref(), "/codebase");
        let slash_root = root.replace('\\', "/");
        if slash_root != root {
            result = result.replace(&slash_root, "/codebase");
        }
        normalize_codebase_paths(&result)
    }

    pub fn rg(
        &self,
        pattern: &str,
        path: &str,
        include: Option<&[String]>,
        exclude: Option<&[String]>,
    ) -> String {
        if pattern.is_empty() {
            return "Error: missing or invalid pattern".to_string();
        }
        if path.is_empty() {
            return "Error: missing or invalid path".to_string();
        }

        self.collected_rg_patterns
            .borrow_mut()
            .push(pattern.to_string());

        let Some(real_path) = self.real(path) else {
            return Self::path_error("path", path);
        };
        if !real_path.exists() {
            return format!("Error: path does not exist: {path}");
        }

        let mut args = vec![
            "--no-heading".to_string(),
            "-n".to_string(),
            "--max-count".to_string(),
            "50".to_string(),
        ];

        if let Some(include) = include {
            for glob in include {
                args.push("--glob".to_string());
                args.push(glob.clone());
            }
        }

        if let Some(exclude) = exclude {
            for glob in exclude {
                for expanded in expand_exclude_globs(glob) {
                    args.push("--glob".to_string());
                    args.push(format!("!{expanded}"));
                }
            }
        }

        args.push("--".to_string());
        args.push(pattern.to_string());
        args.push(real_path.to_string_lossy().into_owned());

        match Command::new(&self.rg_path)
            .args(args.iter().map(OsStr::new))
            .env("RIPGREP_CONFIG_PATH", "")
            .output()
        {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let text = if stdout.is_empty() {
                    "(no matches)"
                } else {
                    &stdout
                };
                self.truncate(&self.remap(text))
            }
            Ok(output) if output.status.code() == Some(1) => "(no matches)".to_string(),
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if !stderr.is_empty() {
                    self.truncate(&self.remap(&stderr))
                } else {
                    format!("Error: rg exited with {}", output.status)
                }
            }
            Err(error) => format!("Error: {error}"),
        }
    }

    pub fn rg_files(
        &self,
        pattern: &str,
        exclude: Option<&[String]>,
        max_matches: usize,
    ) -> Vec<String> {
        if pattern.is_empty() || max_matches == 0 {
            return Vec::new();
        }

        let mut args = vec![
            "-l".to_string(),
            "--max-count".to_string(),
            "10".to_string(),
            "-S".to_string(),
        ];
        if let Some(exclude) = exclude {
            for glob in exclude {
                for expanded in expand_exclude_globs(glob) {
                    args.push("--glob".to_string());
                    args.push(format!("!{expanded}"));
                }
            }
        }
        args.push("--".to_string());
        args.push(pattern.to_string());
        args.push(self.root.to_string_lossy().into_owned());

        let Ok(output) = Command::new(&self.rg_path)
            .args(args.iter().map(OsStr::new))
            .env("RIPGREP_CONFIG_PATH", "")
            .output()
        else {
            return Vec::new();
        };
        if !output.status.success() {
            return Vec::new();
        }

        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let path = PathBuf::from(line.trim());
                let abs = if path.is_absolute() {
                    path
                } else {
                    self.root.join(path)
                };
                let canonical = fs::canonicalize(&abs).unwrap_or(abs);
                if !canonical.starts_with(&self.root) {
                    return None;
                }
                let rel = canonical.strip_prefix(&self.root).ok()?;
                let rel_slash = rel.to_string_lossy().replace('\\', "/");
                if rel_slash.is_empty() {
                    None
                } else {
                    Some(rel_slash)
                }
            })
            .take(max_matches)
            .collect()
    }

    pub fn readfile(
        &self,
        file: &str,
        start_line: Option<usize>,
        end_line: Option<usize>,
    ) -> String {
        if file.is_empty() {
            return "Error: missing or invalid file path".to_string();
        }
        let Some(real_path) = self.real(file) else {
            return Self::path_error("file path", file);
        };

        match fs::metadata(&real_path) {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) | Err(_) => return format!("Error: file not found: {file}"),
        }

        let content = match fs::read_to_string(&real_path) {
            Ok(content) => content,
            Err(error) => return format!("Error: {error}"),
        };

        let all_lines: Vec<&str> = content.split('\n').collect();
        let start = start_line.unwrap_or(1).max(1) - 1;
        let end = end_line.unwrap_or(all_lines.len()).min(all_lines.len());
        if start >= all_lines.len() || start >= end {
            return String::new();
        }

        let out = all_lines[start..end]
            .iter()
            .enumerate()
            .map(|(idx, line)| format!("{}:{line}", start + idx + 1))
            .collect::<Vec<_>>()
            .join("\n");
        self.truncate(&out)
    }

    pub fn tree(&self, path: &str, levels: Option<usize>) -> String {
        if path.is_empty() {
            return "Error: missing or invalid path".to_string();
        }
        let Some(real_path) = self.real(path) else {
            return Self::path_error("path", path);
        };

        match fs::metadata(&real_path) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) | Err(_) => return format!("Error: dir not found: {path}"),
        }

        let mut lines = vec![path.to_string()];
        append_tree_lines(&real_path, "", levels.unwrap_or(usize::MAX), &mut lines);
        self.truncate(&self.remap(&lines.join("\n")))
    }

    pub fn ls(&self, path: &str, long_format: bool, all_files: bool) -> String {
        if path.is_empty() {
            return "Error: missing or invalid path".to_string();
        }
        let Some(real_path) = self.real(path) else {
            return Self::path_error("path", path);
        };

        match fs::metadata(&real_path) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return format!("Error: not a directory: {path}"),
            Err(_) => return format!("Error: dir not found: {path}"),
        }

        let mut entries = match fs::read_dir(&real_path) {
            Ok(read_dir) => read_dir
                .filter_map(Result::ok)
                .filter(|entry| all_files || !entry.file_name().to_string_lossy().starts_with('.'))
                .collect::<Vec<_>>(),
            Err(error) => return format!("Error: {error}"),
        };
        entries.sort_by_key(|entry| entry.file_name());

        if !long_format {
            let out = entries
                .iter()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("\n");
            return self.truncate(&out);
        }

        let mut lines = vec![format!("total {}", entries.len())];
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            match entry.metadata() {
                Ok(metadata) => {
                    let kind = if metadata.is_dir() { 'd' } else { '-' };
                    let size = metadata.len();
                    let (month, day, hh, mm) = metadata
                        .modified()
                        .ok()
                        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                        .map(rough_utc_timestamp)
                        .unwrap_or(("Jan", 1, 0, 0));
                    lines.push(format!(
                        "{kind}rwxr-xr-x  1 user  staff {size:>8} {month} {day:02} {hh:02}:{mm:02} {name}"
                    ));
                }
                Err(_) => lines.push(format!("?---------  ? ?     ?        ? ? ?     ? {name}")),
            }
        }
        self.truncate(&self.remap(&lines.join("\n")))
    }

    pub fn glob(&self, pattern: &str, path: &str, type_filter: &str) -> String {
        if pattern.is_empty() {
            return "Error: missing or invalid pattern".to_string();
        }
        if path.is_empty() {
            return "Error: missing or invalid path".to_string();
        }
        let Some(real_path) = self.real(path) else {
            return Self::path_error("path", path);
        };

        let matcher = GlobMatcherPair::new(pattern);
        let recursive = pattern.contains("**");
        let max_depth = if recursive { usize::MAX } else { 1 };
        let mut matches = Vec::new();

        let walker = WalkDir::new(&real_path)
            .min_depth(1)
            .max_depth(max_depth)
            .into_iter()
            .filter_entry(|entry| should_descend(entry, recursive));

        for entry in walker.filter_map(Result::ok) {
            if matches.len() >= 100 {
                break;
            }
            let path = entry.path();
            let rel = path.strip_prefix(&real_path).unwrap_or(path);
            let rel_slash = rel.to_string_lossy().replace('\\', "/");
            let name = entry.file_name().to_string_lossy();
            if !matcher.is_match(&rel_slash, &name) {
                continue;
            }
            match type_filter {
                "file" if !entry.file_type().is_file() => continue,
                "directory" if !entry.file_type().is_dir() => continue,
                _ => {}
            }
            matches.push(path.to_path_buf());
        }

        matches.sort();
        matches.truncate(100);
        let out = matches
            .iter()
            .map(|path| self.remap(&path.to_string_lossy()))
            .collect::<Vec<_>>()
            .join("\n");
        if out.is_empty() {
            "(no matches)".to_string()
        } else {
            out
        }
    }

    pub fn exec_command(&self, cmd: &ToolCommand) -> String {
        match cmd.command_type.as_str() {
            "rg" => self.rg(
                cmd.pattern.as_deref().unwrap_or_default(),
                cmd.path.as_deref().unwrap_or_default(),
                cmd.include.as_deref(),
                cmd.exclude.as_deref(),
            ),
            "readfile" => self.readfile(
                cmd.file.as_deref().unwrap_or_default(),
                cmd.start_line,
                cmd.end_line,
            ),
            "tree" => self.tree(cmd.path.as_deref().unwrap_or_default(), cmd.levels),
            "ls" => self.ls(
                cmd.path.as_deref().unwrap_or_default(),
                cmd.long_format,
                cmd.all_files,
            ),
            "glob" => self.glob(
                cmd.pattern.as_deref().unwrap_or_default(),
                cmd.path.as_deref().unwrap_or_default(),
                cmd.type_filter.as_deref().unwrap_or("all"),
            ),
            other => format!("Error: unknown command type '{other}'"),
        }
    }

    pub fn exec_tool_call(&self, args: &Value) -> String {
        let Some(object) = args.as_object() else {
            return "Error: missing or invalid tool args".to_string();
        };

        let command_key = Regex::new(r"^command\d+$").expect("valid command key regex");
        let mut keys = object
            .keys()
            .filter(|key| command_key.is_match(key))
            .collect::<Vec<_>>();
        keys.sort_by(|a, b| compare_command_keys(a, b));

        let mut parts = Vec::new();
        for key in keys {
            let output = object
                .get(key)
                .cloned()
                .and_then(|value| serde_json::from_value::<ToolCommand>(value).ok())
                .map(|cmd| self.exec_command(&cmd))
                .unwrap_or_else(|| "Error: missing or invalid command".to_string());
            parts.push(format!("<{key}_result>\n{output}\n</{key}_result>"));
        }
        parts.join("")
    }
}

fn normalize_codebase_paths(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        output.push_str(&normalize_codebase_paths_in_line(line));
    }
    if !text.ends_with('\n') && !text.is_empty() {
        let last_line_start = text.rfind('\n').map(|idx| idx + 1).unwrap_or(0);
        if last_line_start == text.len() {
            return output;
        }
    }
    output
}

fn normalize_codebase_paths_in_line(line: &str) -> String {
    const PREFIX: &str = "/codebase";

    let mut output = String::with_capacity(line.len());
    let mut cursor = 0;
    while let Some(relative_start) = line[cursor..].find(PREFIX) {
        let start = cursor + relative_start;
        output.push_str(&line[cursor..start]);

        let after_prefix = start + PREFIX.len();
        let rest = &line[after_prefix..];
        let path_tail_len = rest
            .char_indices()
            .find_map(|(idx, ch)| {
                if ch == '\n' || ch == '\r' {
                    Some(idx)
                } else if ch == ':' {
                    let after_colon = &rest[idx + ch.len_utf8()..];
                    if after_colon
                        .chars()
                        .next()
                        .is_some_and(|next| next.is_ascii_digit())
                    {
                        Some(idx)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .unwrap_or(rest.len());

        let path_end = after_prefix + path_tail_len;
        output.push_str(&line[start..path_end].replace('\\', "/"));
        cursor = path_end;
    }
    output.push_str(&line[cursor..]);
    output
}

fn truncate_with_limits(text: &str, limits: &ExecutorLimits) -> String {
    let lines = text.split('\n').collect::<Vec<_>>();
    let mut truncated_lines = Vec::new();
    let limit = lines.len().min(limits.result_max_lines);
    for line in lines.iter().take(limit) {
        if line.chars().count() > limits.line_max_chars {
            truncated_lines.push(line.chars().take(limits.line_max_chars).collect::<String>());
        } else {
            truncated_lines.push((*line).to_string());
        }
    }
    let mut result = truncated_lines.join("\n");
    if lines.len() > limits.result_max_lines {
        result.push_str("\n... (lines truncated) ...");
    }
    result
}

fn clean_relative_path(rel: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in Path::new(rel).components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out)
}

fn is_within_lexically(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}

fn expand_exclude_globs(pattern: &str) -> Vec<String> {
    let normalized = pattern.trim().replace('\\', "/");
    if normalized.is_empty() {
        return Vec::new();
    }
    let mut expanded = vec![normalized.clone()];
    if !normalized.starts_with("**/") && !normalized.starts_with('/') {
        expanded.push(format!("**/{normalized}"));
    }
    expanded.sort();
    expanded.dedup();
    expanded
}

fn append_tree_lines(dir: &Path, prefix: &str, depth_remaining: usize, lines: &mut Vec<String>) {
    if depth_remaining == 0 {
        return;
    }

    let mut entries = match fs::read_dir(dir) {
        Ok(read_dir) => read_dir.filter_map(Result::ok).collect::<Vec<_>>(),
        Err(_) => return,
    };
    entries.sort_by_key(|entry| entry.file_name());

    let count = entries.len();
    for (idx, entry) in entries.into_iter().enumerate() {
        let is_last = idx + 1 == count;
        let connector = if is_last { "└── " } else { "├── " };
        let name = entry.file_name().to_string_lossy().into_owned();
        lines.push(format!("{prefix}{connector}{name}"));
        if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            let next_prefix = format!("{prefix}{}", if is_last { "    " } else { "│   " });
            append_tree_lines(&entry.path(), &next_prefix, depth_remaining - 1, lines);
        }
    }
}

fn should_descend(entry: &DirEntry, recursive: bool) -> bool {
    if entry.depth() == 0 {
        return true;
    }
    recursive || !entry.file_name().to_string_lossy().starts_with('.')
}

struct GlobMatcherPair {
    set: Option<GlobSet>,
    pattern: String,
}

impl GlobMatcherPair {
    fn new(pattern: &str) -> Self {
        let mut builder = GlobSetBuilder::new();
        let set = Glob::new(pattern)
            .map(|glob| {
                builder.add(glob);
                builder.build().ok()
            })
            .unwrap_or(None);
        Self {
            set,
            pattern: pattern.to_string(),
        }
    }

    fn is_match(&self, rel: &str, name: &str) -> bool {
        if let Some(set) = &self.set
            && (set.is_match(rel) || set.is_match(name))
        {
            return true;
        }
        fnmatch(rel, &self.pattern) || fnmatch(name, &self.pattern)
    }
}

fn fnmatch(str_value: &str, pattern: &str) -> bool {
    let mut regex = String::from("^");
    let mut chars = pattern.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '*' => {
                if chars.peek() == Some(&'*') {
                    chars.next();
                    regex.push_str(".*");
                    if chars.peek() == Some(&'/') {
                        chars.next();
                    }
                } else {
                    regex.push_str("[^/]*");
                }
            }
            '?' => regex.push_str("[^/]"),
            '[' => regex.push('['),
            ']' => regex.push(']'),
            '.' | '+' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '\\' => {
                regex.push('\\');
                regex.push(ch);
            }
            other => regex.push(other),
        }
    }
    regex.push('$');
    Regex::new(&regex)
        .map(|regex| regex.is_match(str_value))
        .unwrap_or(false)
}

fn compare_command_keys(a: &str, b: &str) -> Ordering {
    let a_num = a
        .trim_start_matches("command")
        .parse::<usize>()
        .unwrap_or(0);
    let b_num = b
        .trim_start_matches("command")
        .parse::<usize>()
        .unwrap_or(0);
    a_num.cmp(&b_num)
}

fn rough_utc_timestamp(duration: Duration) -> (&'static str, u64, u64, u64) {
    let total_minutes = duration.as_secs() / 60;
    let mm = total_minutes % 60;
    let total_hours = total_minutes / 60;
    let hh = total_hours % 24;
    let total_days = total_hours / 24;
    let day = (total_days % 28) + 1;
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][((total_days / 28) % 12) as usize];
    (month, day, hh, mm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_exclude_like_node() {
        assert_eq!(
            expand_exclude_globs("vendor/**"),
            vec!["**/vendor/**", "vendor/**"]
        );
        assert_eq!(expand_exclude_globs("**/*.test.js"), vec!["**/*.test.js"]);
    }

    #[test]
    fn command_key_ordering_is_numeric() {
        let mut keys = ["command10", "command2", "command1"];
        keys.sort_by(|a, b| compare_command_keys(a, b));
        assert_eq!(keys, ["command1", "command2", "command10"]);
    }

    #[test]
    fn normalizes_codebase_paths_to_forward_slashes() {
        assert_eq!(
            normalize_codebase_paths("/codebase\\src\\app.js:1:needle\n/codebase\\vendor\\lib.js"),
            "/codebase/src/app.js:1:needle\n/codebase/vendor/lib.js"
        );
    }
}
