use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Context-trimming proxy for MCP servers.
#[derive(Debug, Parser)]
#[command(name = "meiose", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run an MCP server behind the proxy: `meiose run -- npx @playwright/mcp`
    Run(RunArgs),
    /// Like `run`, but also log every message to a JSONL recording
    Record(RecordArgs),
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Forward everything untouched (disables trimming; useful for debugging)
    #[arg(long)]
    pub passthrough: bool,

    /// The MCP server command and its arguments, after `--`
    #[arg(last = true, required = true, value_name = "CMD")]
    pub cmd: Vec<String>,
}

#[derive(Debug, Args)]
pub struct RecordArgs {
    /// Directory for the recording [default: <data dir>/meiose/recordings]
    #[arg(long, value_name = "DIR")]
    pub out: Option<PathBuf>,

    /// The MCP server command and its arguments, after `--`
    #[arg(last = true, required = true, value_name = "CMD")]
    pub cmd: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_takes_everything_after_double_dash() {
        let cli = Cli::try_parse_from([
            "meiose",
            "run",
            "--",
            "npx",
            "@playwright/mcp",
            "--headless",
        ])
        .unwrap();
        let Command::Run(args) = cli.command else {
            panic!("expected run")
        };
        assert_eq!(args.cmd, ["npx", "@playwright/mcp", "--headless"]);
        assert!(!args.passthrough);
    }

    #[test]
    fn run_requires_a_command() {
        assert!(Cli::try_parse_from(["meiose", "run"]).is_err());
    }

    #[test]
    fn clap_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
