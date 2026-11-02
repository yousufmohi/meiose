use anyhow::Context;
use clap::Parser;

use meiose::cli::{Cli, Command};
use meiose::{logging, paths, proxy, record};

fn main() {
    let cli = Cli::parse();
    let log_guard = logging::init();

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!(
                "meiose: could not start the async runtime: {e}\n  → this is a bug; please report it"
            );
            std::process::exit(1);
        }
    };
    let code = match rt.block_on(dispatch(cli)) {
        Ok(code) => code,
        Err(e) => {
            tracing::error!("{e:#}");
            eprintln!("meiose: {e:#}");
            1
        }
    };

    // Flush logs, then exit while `rt` is still alive. Dropping the runtime
    // would wait for the blocking stdin read, which never returns while the
    // agent keeps our stdin open (e.g. when the server exits first).
    drop(log_guard);
    std::process::exit(code);
}

async fn dispatch(cli: Cli) -> anyhow::Result<i32> {
    match cli.command {
        Command::Run(args) => {
            if args.passthrough {
                tracing::info!("--passthrough: trimming disabled");
            }
            proxy::run(proxy::Options {
                cmd: args.cmd,
                recorder: None,
                grace: proxy::default_grace(),
            })
            .await
        }
        Command::Record(args) => {
            let dir = match args.out {
                Some(d) => d,
                None => paths::recordings_dir().context(
                    "could not find a data directory for recordings\n  → pass one with --out <DIR>",
                )?,
            };
            let (recorder, rec_task, path) = record::Recorder::create(&dir, &args.cmd).await?;
            // stderr, not stdout: stdout carries only protocol messages.
            eprintln!("meiose: recording to {}", path.display());
            tracing::info!(path = %path.display(), "recording");

            let code = proxy::run(proxy::Options {
                cmd: args.cmd,
                recorder: Some(recorder),
                grace: proxy::default_grace(),
            })
            .await?;

            match rec_task.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => eprintln!(
                    "meiose: the recording at {} may be incomplete: {e}\n  → check free disk space",
                    path.display()
                ),
                Err(e) => tracing::error!(error = %e, "recorder task failed"),
            }
            Ok(code)
        }
    }
}
