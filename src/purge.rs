//! `meiose purge`: delete what meiose keeps that can hold secrets — stored
//! original outputs, logs, recordings in the data directory, and config
//! backups left over from earlier installs. Call stats are kept.
//!
//! Backups of configs that are still wrapped are never deleted: they are how
//! `meiose uninstall` restores those files. Run `uninstall` first.

use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::cli::PurgeArgs;
use crate::{logging, paths, report, setup, store};

struct Plan {
    data: PathBuf,
    /// (stored outputs, their bytes), if the database exists.
    outputs: Option<(i64, i64)>,
    logs: Vec<PathBuf>,
    recordings: Vec<PathBuf>,
    leftover_backups: Vec<PathBuf>,
    /// Config files whose backups are kept because they are still wrapped.
    still_wrapped: Vec<PathBuf>,
}

impl Plan {
    fn is_empty(&self) -> bool {
        self.outputs.is_none_or(|(n, _)| n == 0)
            && self.logs.is_empty()
            && self.recordings.is_empty()
            && self.leftover_backups.is_empty()
    }
}

fn files(dir: &Path, keep: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
                .filter(|e| keep(&e.file_name().to_string_lossy()))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn plan(data: &Path) -> anyhow::Result<Plan> {
    let db = data.join("meiose.db");
    let outputs = if db.is_file() {
        let conn = store::open_db(&db)?;
        Some(store::output_totals(&conn)?)
    } else {
        None
    };
    let dir = setup::install_dir()?;
    let manifest = dir.load()?;
    let in_use: Vec<&PathBuf> = manifest
        .files
        .iter()
        .flat_map(|f| [&f.backup, &f.written])
        .collect();
    let leftover_backups = files(&dir.dir, |n| {
        n.ends_with(".backup.json") || n.ends_with(".written.json")
    })
    .into_iter()
    .filter(|p| !in_use.contains(&p))
    .collect();
    Ok(Plan {
        data: data.to_path_buf(),
        outputs,
        logs: files(&data.join("logs"), logging::is_log_file),
        recordings: files(&data.join("recordings"), |n| n.ends_with(".jsonl")),
        leftover_backups,
        still_wrapped: manifest.files.iter().map(|f| f.path.clone()).collect(),
    })
}

pub fn run(args: PurgeArgs) -> anyhow::Result<i32> {
    let data = paths::data_dir().context(
        "could not find the meiose data directory\n  → set MEIOSE_DATA_DIR to where meiose keeps its data",
    )?;
    if !data.is_dir() {
        println!("Nothing to purge: {} does not exist.", data.display());
        return Ok(0);
    }
    let p = plan(&data)?;
    if p.is_empty() {
        println!("Nothing to purge in {}.", p.data.display());
    } else {
        println!("meiose purge will delete, in {}:", p.data.display());
        if let Some((n, bytes)) = p.outputs.filter(|(n, _)| *n > 0) {
            println!(
                "  · {n} stored output(s), {}B, from meiose.db (call stats are kept; \
                 `meiose_expand` and `log --show` will not find them)",
                report::short(bytes)
            );
        }
        let lists = [
            (&p.logs, "log file(s) in logs/"),
            (&p.recordings, "recording(s) in recordings/"),
            (&p.leftover_backups, "leftover backup(s) in install/"),
        ];
        for (list, what) in lists {
            if !list.is_empty() {
                println!("  · {} {what}", list.len());
            }
        }
        println!("  Recordings saved elsewhere with --out are not touched.");
    }
    if !p.still_wrapped.is_empty() {
        println!(
            "Kept: the backups of {} config file(s) meiose still wraps, which `meiose uninstall` \
             needs to restore them:",
            p.still_wrapped.len()
        );
        for f in &p.still_wrapped {
            println!("  · {}", f.display());
        }
        println!("  → run `meiose uninstall` first; it deletes them once the files are restored.");
    }
    if p.is_empty() {
        return Ok(0);
    }
    if !args.yes && !setup::confirm("\nDelete these now?") {
        println!("Nothing deleted. (Re-run with --yes to skip this question.)");
        return Ok(1);
    }

    let mut failed = false;
    if p.outputs.is_some_and(|(n, _)| n > 0) {
        match store::open_db(&p.data.join("meiose.db")).and_then(|c| Ok(store::purge_outputs(&c)?))
        {
            Ok(n) => println!("✓ deleted {n} stored output(s)"),
            Err(e) => {
                failed = true;
                println!("✗ could not delete the stored outputs: {e:#}");
            }
        }
    }
    for (list, what) in [
        (&p.logs, "log file(s)"),
        (&p.recordings, "recording(s)"),
        (&p.leftover_backups, "leftover backup(s)"),
    ] {
        if list.is_empty() {
            continue;
        }
        let mut n = 0;
        for f in list {
            match std::fs::remove_file(f) {
                Ok(()) => n += 1,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => n += 1,
                Err(e) => {
                    failed = true;
                    println!("✗ could not delete {}: {e}", f.display());
                }
            }
        }
        println!("✓ deleted {n} {what}");
    }
    if failed {
        println!(
            "\nSome files could not be deleted.\n  → close your agents (a running meiose keeps its \
             log open), then run `meiose purge` again."
        );
    } else {
        println!(
            "\nMeiose processes that are still running keep new outputs in memory and on disk as \
             usual; restart your agents if you want those gone too."
        );
    }
    Ok(i32::from(failed))
}
