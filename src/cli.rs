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
    /// Token savings over a time window
    Stats(StatsArgs),
    /// Recent tool calls, raw vs trimmed
    Log(LogArgs),
}

/// Options shared by `run` and `record`.
#[derive(Debug, Args, Clone, Copy, Default)]
pub struct ProxyFlags {
    /// Forward everything untouched (disables trimming; useful for debugging)
    #[arg(long)]
    pub passthrough: bool,

    /// Trim harder: hide every non-interactive ref, drop link URLs, shorten more
    #[arg(long, conflicts_with = "passthrough")]
    pub aggressive: bool,

    /// Always send full snapshots, never a diff against the previous one
    #[arg(long)]
    pub no_diff: bool,
}

#[derive(Debug, Args)]
pub struct StatsArgs {
    /// How many days back to count
    #[arg(long, default_value_t = 7)]
    pub days: u32,
}

#[derive(Debug, Args)]
pub struct LogArgs {
    /// How many recent calls to list
    #[arg(short = 'n', long, default_value_t = 20)]
    pub limit: usize,

    /// Print the trimmed and original output for an expand id
    #[arg(long, value_name = "ID")]
    pub show: Option<String>,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    #[command(flatten)]
    pub flags: ProxyFlags,

    /// The MCP server command and its arguments, after `--`
    #[arg(last = true, required = true, value_name = "CMD")]
    pub cmd: Vec<String>,
}

#[derive(Debug, Args)]
pub struct RecordArgs {
    /// Directory for the recording [default: <data dir>/meiose/recordings]
    #[arg(long, value_name = "DIR")]
    pub out: Option<PathBuf>,

    #[command(flatten)]
    pub flags: ProxyFlags,

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
        assert!(!args.flags.passthrough);
        assert!(!args.flags.aggressive);
    }

    #[test]
    fn flags_parse_before_double_dash() {
        let cli = Cli::try_parse_from([
            "meiose",
            "run",
            "--aggressive",
            "--no-diff",
            "--",
            "srv",
            "--aggressive",
        ])
        .unwrap();
        let Command::Run(args) = cli.command else {
            panic!("expected run")
        };
        assert!(args.flags.aggressive && args.flags.no_diff);
        assert_eq!(args.cmd, ["srv", "--aggressive"]);
        assert!(
            Cli::try_parse_from(["meiose", "run", "--passthrough", "--aggressive", "--", "x"])
                .is_err()
        );
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
