use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use fast_context_rust::{config, extract_key, mcp};

#[derive(Debug, Parser)]
#[command(name = "fast-context-rust")]
#[command(about = "Rust fast-context MCP implementation", version)]
struct Cli {
    /// Only check that ripgrep is available, print its version, then exit.
    #[arg(long)]
    check_rg: bool,

    /// Extract Windsurf API key from state.vscdb, print it, then exit without starting MCP.
    #[arg(long)]
    extract_windsurf_key: bool,

    /// SQLite database path for --extract-windsurf-key. Defaults to Windsurf state.vscdb.
    #[arg(long, value_name = "PATH")]
    db_path: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    if cli.extract_windsurf_key {
        let db_path = match cli.db_path {
            Some(path) => path,
            None => match extract_key::default_windsurf_db_path() {
                Ok(path) => path,
                Err(error) => {
                    eprintln!("{}", error.user_message());
                    return ExitCode::from(1);
                }
            },
        };

        match extract_key::extract_key_from_db_path(&db_path) {
            Ok(key) => {
                println!("{key}");
                return ExitCode::SUCCESS;
            }
            Err(error) => {
                eprintln!("{}", error.user_message());
                return ExitCode::from(1);
            }
        }
    }

    match config::preflight_ripgrep() {
        Ok(check) => {
            if cli.check_rg {
                println!("{}", check.version);
                return ExitCode::SUCCESS;
            }
        }
        Err(error) => {
            eprintln!("{}", error.user_message());
            return ExitCode::from(1);
        }
    }

    match mcp::serve_stdio().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}", error.user_message());
            ExitCode::from(1)
        }
    }
}
