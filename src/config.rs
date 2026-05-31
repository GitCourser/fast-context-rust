use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use thiserror::Error;

pub const FC_RG_PATH_ENV: &str = "FC_RG_PATH";
pub const RESULT_MAX_LINES_ENV: &str = "FC_RESULT_MAX_LINES";
pub const LINE_MAX_CHARS_ENV: &str = "FC_LINE_MAX_CHARS";

pub const DEFAULT_RESULT_MAX_LINES: usize = 50;
pub const DEFAULT_LINE_MAX_CHARS: usize = 250;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutorLimits {
    pub result_max_lines: usize,
    pub line_max_chars: usize,
}

impl ExecutorLimits {
    pub fn from_env() -> Self {
        Self {
            result_max_lines: read_int_env(RESULT_MAX_LINES_ENV, DEFAULT_RESULT_MAX_LINES, 1, 500),
            line_max_chars: read_int_env(LINE_MAX_CHARS_ENV, DEFAULT_LINE_MAX_CHARS, 20, 10_000),
        }
    }
}

impl Default for ExecutorLimits {
    fn default() -> Self {
        Self {
            result_max_lines: DEFAULT_RESULT_MAX_LINES,
            line_max_chars: DEFAULT_LINE_MAX_CHARS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RipgrepCheck {
    pub path: PathBuf,
    pub version: String,
}

#[derive(Debug, Error)]
pub enum RipgrepError {
    #[error("ripgrep (rg) not found or not executable: {path}")]
    NotFound { path: String, reason: String },
    #[error("ripgrep (rg) was found but failed version check: {path}")]
    VersionCheckFailed {
        path: String,
        status: String,
        stderr: String,
    },
}

impl RipgrepError {
    pub fn user_message(&self) -> String {
        let details = match self {
            Self::NotFound { path, reason } => {
                format!("ripgrep (rg) not found or not executable: {path}\n[diagnostic] {reason}")
            }
            Self::VersionCheckFailed {
                path,
                status,
                stderr,
            } => format!(
                "ripgrep (rg) was found but failed version check: {path}\n[diagnostic] status={status}, stderr={stderr}"
            ),
        };

        format!(
            "{details}\n\nfast-context-rust requires ripgrep before MCP startup. Install ripgrep and ensure `rg` is visible in PATH, or set FC_RG_PATH to the rg executable.\nInstall examples: macOS `brew install ripgrep`; Debian/Ubuntu `sudo apt-get install ripgrep`; Fedora `sudo dnf install ripgrep`; Windows `winget install BurntSushi.ripgrep.MSVC` or `choco install ripgrep`."
        )
    }
}

pub fn read_int_env(name: &str, default_value: usize, min: usize, max: usize) -> usize {
    let Ok(raw) = env::var(name) else {
        return default_value;
    };
    let Ok(parsed) = raw.trim().parse::<usize>() else {
        return default_value;
    };
    parsed.clamp(min, max)
}

pub fn read_u64_env(name: &str, default_value: u64, min: u64, max: u64) -> u64 {
    let Ok(raw) = env::var(name) else {
        return default_value;
    };
    let Ok(parsed) = raw.trim().parse::<u64>() else {
        return default_value;
    };
    parsed.clamp(min, max)
}

pub fn read_bool_env(name: &str, default_value: bool) -> bool {
    let Ok(raw) = env::var(name) else {
        return default_value;
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => default_value,
    }
}

pub fn is_env_set(name: &str) -> bool {
    env::var(name).is_ok()
}

pub fn resolve_ripgrep_path() -> Result<PathBuf, RipgrepError> {
    let env_path = env::var_os(FC_RG_PATH_ENV);
    resolve_ripgrep_path_with(env_path.as_deref())
}

pub fn resolve_ripgrep_path_with(fc_rg_path: Option<&OsStr>) -> Result<PathBuf, RipgrepError> {
    if let Some(path) = fc_rg_path
        && !path.is_empty()
    {
        return Ok(PathBuf::from(path));
    }

    which::which("rg").map_err(|err| RipgrepError::NotFound {
        path: "rg".to_string(),
        reason: err.to_string(),
    })
}

pub fn preflight_ripgrep() -> Result<RipgrepCheck, RipgrepError> {
    let path = resolve_ripgrep_path()?;
    check_ripgrep_path(&path)
}

pub fn check_ripgrep_path(path: &Path) -> Result<RipgrepCheck, RipgrepError> {
    let output = Command::new(path)
        .arg("--version")
        .env("RIPGREP_CONFIG_PATH", "")
        .output()
        .map_err(|err| RipgrepError::NotFound {
            path: path.display().to_string(),
            reason: err.to_string(),
        })?;

    if !output.status.success() {
        return Err(RipgrepError::VersionCheckFailed {
            path: path.display().to_string(),
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let version = stdout.lines().next().unwrap_or_default().to_string();
    Ok(RipgrepCheck {
        path: path.to_path_buf(),
        version,
    })
}
