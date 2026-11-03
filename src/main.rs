use std::sync::Arc;

use anyhow::Context;
use clap::Parser;

use meiose::cli::{Cli, Command, ProxyFlags};
use meiose::handlers::Mode;
use meiose::intercept::{Config, Interceptor};
use meiose::store::{self, StoreHandle};
use meiose::{doctor, logging, paths, proxy, purge, record, report, secure, setup};

fn main() {
    let cli = Cli::parse();
    // `purge` deletes the logs, so it does not open one.
    let log_guard = if matches!(cli.command, Command::Purge(_)) {
        None
    } else {
        logging::init()
    };
    // Installs from before 0.3 created group- and world-readable files.
    if let Some(data) = paths::data_dir() {
        secure::tighten_data_dir(&data);
    }

    // The proxy mostly waits on pipes and trims in a few milliseconds, so two
    // workers are plenty; the default (one per core) only adds idle threads,
    // and every wrapped MCP server runs its own meiose.
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("meiose")
        .enable_all()
        .build()
    {
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

/// Build the interceptor for `run`/`record`. Opening the store can fail
/// (read-only disk, permissions): then we proxy without trimming, because
/// we must never trim an output we could not give back.
fn interceptor(flags: ProxyFlags, cmd: &[String]) -> (Arc<Interceptor>, Option<StoreHandle>) {
    let store = match store::default_path() {
        Some(path) => match StoreHandle::open(&path) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::warn!("{e:#}");
                eprintln!("meiose: trimming disabled for this session: {e:#}");
                None
            }
        },
        None => {
            eprintln!(
                "meiose: trimming disabled: no data directory found\n  → set MEIOSE_DATA_DIR to a writable directory"
            );
            None
        }
    };
    if flags.passthrough {
        tracing::info!("--passthrough: trimming disabled");
    }
    let cfg = Config {
        mode: if flags.aggressive {
            Mode::Aggressive
        } else {
            Mode::Conservative
        },
        trim: !flags.passthrough,
        diff: !flags.no_diff,
        // ~3 bytes per token, on the safe side of the ~3.7 seen on real pages.
        // Counting exactly would put tiktoken on the message path.
        budget_bytes: flags.budget.saturating_mul(3),
        server: record::file_stem(cmd),
        server_key: store::server_key(cmd),
    };
    (Arc::new(Interceptor::new(cfg, store.clone())), store)
}

async fn run_proxy(
    cmd: Vec<String>,
    flags: ProxyFlags,
    recorder: Option<record::Recorder>,
) -> anyhow::Result<i32> {
    let (intercept, store) = interceptor(flags, &cmd);
    let code = proxy::run(proxy::Options {
        cmd,
        recorder,
        intercept: Some(intercept),
        grace: proxy::default_grace(),
    })
    .await?;
    if let Some(s) = store {
        // Persist the last stats rows and close the database before we exit.
        let _ = tokio::task::spawn_blocking(move || s.close()).await;
    }
    Ok(code)
}

async fn dispatch(cli: Cli) -> anyhow::Result<i32> {
    match cli.command {
        Command::Run(args) => run_proxy(args.cmd, args.flags, None).await,
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

            let code = run_proxy(args.cmd, args.flags, Some(recorder)).await?;

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
        Command::Stats(args) => {
            let conn = open_existing_db()?;
            let since = store::now_ms() - i64::from(args.days) * 24 * 3600 * 1000;
            let totals = store::totals_since(&conn, since)?;
            let per_tool = store::per_tool_since(&conn, since)?;
            emit(&report::stats(args.days, &totals, &per_tool));
            Ok(0)
        }
        Command::Log(args) => {
            let conn = open_existing_db()?;
            if let Some(id) = args.show {
                let Some((original, trimmed)) = store::output_pair(&conn, &id)? else {
                    anyhow::bail!(
                        "no stored output with id {id:?}\n  → ids are listed by `meiose log`; outputs are kept for 7 days"
                    );
                };
                emit(&format!(
                    "=== trimmed (what the agent saw) ===\n{}\n\n=== original (what meiose_expand returns) ===\n{}\n",
                    report::result_text(&trimmed),
                    report::result_text(&original)
                ));
                return Ok(0);
            }
            let rows = store::recent_calls(&conn, args.limit)?;
            emit(&report::log(store::now_ms(), &rows));
            Ok(0)
        }
        Command::Install(args) => setup::install(args),
        Command::Uninstall(args) => setup::uninstall(args),
        Command::Purge(args) => purge::run(args),
        Command::Doctor => {
            let roots = setup::roots()?;
            let checks = doctor::run(&roots);
            emit(&doctor::render(&checks));
            Ok(i32::from(
                checks.iter().any(|c| c.status == doctor::Status::Fail),
            ))
        }
    }
}

/// Print report text. Pages and servers put whatever they like into tool
/// output and names, so control characters (terminal escapes such as an
/// OSC 52 clipboard write) are removed here, at output time; the stored
/// originals, and `meiose_expand`, stay byte-for-byte. A reader that goes
/// away (`meiose log --show ID | head`) ends the output quietly.
fn emit(text: &str) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = out
        .write_all(report::printable(text).as_bytes())
        .and_then(|()| out.flush());
}

fn open_existing_db() -> anyhow::Result<rusqlite::Connection> {
    let path = store::default_path().context(
        "could not find the meiose data directory\n  → set MEIOSE_DATA_DIR to where meiose keeps its data",
    )?;
    store::open_db(&path)
}
