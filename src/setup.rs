//! The interactive side of `meiose install` / `meiose uninstall`: show the
//! plan, ask, apply, report. The mechanics live in [`crate::install`].

use std::io::{BufRead, Write};

use anyhow::Context;

use crate::cli::{ClientArg, InstallArgs, UninstallArgs};
use crate::install::{self, Client, FilePlan, InstallDir, Outcome, Roots, Skip, Undo};

/// How often a file may change under us before we give up on it.
const MAX_REPLANS: usize = 5;

pub fn roots() -> anyhow::Result<Roots> {
    Roots::from_env().context(
        "could not find your home directory\n  → set HOME (or USERPROFILE on Windows) and try again",
    )
}

pub fn install_dir() -> anyhow::Result<InstallDir> {
    let data = crate::paths::data_dir().context(
        "could not find a data directory for backups\n  → set MEIOSE_DATA_DIR to a writable directory",
    )?;
    Ok(InstallDir::new(&data))
}

/// Ask a yes/no question on the terminal. EOF or anything but y/yes is "no".
pub fn confirm(question: &str) -> bool {
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

pub fn install(args: InstallArgs) -> anyhow::Result<i32> {
    let roots = roots()?;
    let meiose = install::meiose_path(args.meiose_path.as_deref())?;
    let clients: Vec<Client> = args
        .client
        .iter()
        .map(|c| match c {
            ClientArg::ClaudeCode => Client::ClaudeCode,
            ClientArg::ClaudeDesktop => Client::ClaudeDesktop,
            ClientArg::Cursor => Client::Cursor,
        })
        .collect();

    let plans = install::plan_all(&roots, &meiose, &args.only, &clients);
    if plans.is_empty() {
        println!(
            "No agent configs found (looked for Claude Code, Claude Desktop and Cursor).\n  \
             → add an MCP server in your agent first, or wrap one by hand: \
             set its command to `{meiose}` and put `run --` before its original command and args."
        );
        return Ok(0);
    }

    let mut ready = Vec::new();
    let mut failed = false;
    for (client, path, plan) in plans {
        println!("{} — {}", client.label(), path.display());
        match plan {
            Err(e) => {
                println!("  ✗ {e:#}\n");
                failed = true;
            }
            Ok(fp) => {
                print_plan(&fp);
                if fp.plan.changes.is_empty() {
                    println!("  nothing to change");
                } else {
                    ready.push(fp);
                }
                println!();
            }
        }
    }

    if ready.is_empty() {
        println!("Nothing to do: every stdio server already goes through meiose.");
        return Ok(i32::from(failed));
    }
    if args.dry_run {
        println!("Dry run: nothing was changed.");
        return Ok(i32::from(failed));
    }
    let n: usize = ready.iter().map(|f| f.plan.changes.len()).sum();
    if !args.yes
        && !confirm(&format!(
            "Wrap {n} server(s) in {} file(s)? Each file is backed up first.",
            ready.len()
        ))
    {
        println!("Nothing changed. (Re-run with --yes to skip this question.)");
        return Ok(1);
    }

    let dir = install_dir()?;
    let mut manifest = dir.load()?;
    let mut hints = Vec::new();
    for mut fp in ready {
        // The question above may have stayed open for minutes, and Claude
        // Code rewrites its file all the time: a plan is only applied to the
        // file it was computed from. Otherwise plan again from the current
        // content, and ask again if that changes what would be done.
        let mut replans = 0;
        loop {
            match install::apply(&dir, &mut manifest, &fp) {
                Ok(Outcome::Applied) => {
                    println!("✓ updated {}", fp.path.display());
                    if !hints.contains(&fp.client) {
                        hints.push(fp.client);
                    }
                    break;
                }
                Ok(Outcome::FileChanged) if replans >= MAX_REPLANS => {
                    println!(
                        "✗ {}: keeps changing while meiose tries to write it\n  → close the app that is writing it and run `meiose install` again",
                        fp.path.display()
                    );
                    failed = true;
                    break;
                }
                Ok(Outcome::FileChanged) => {
                    replans += 1;
                    match install::plan_file(fp.client, &fp.path, &meiose, &args.only) {
                        Err(e) => {
                            println!("✗ {} changed while waiting: {e:#}", fp.path.display());
                            failed = true;
                            break;
                        }
                        Ok(fresh) if fresh.plan.changes.is_empty() => {
                            println!(
                                "· {} changed while waiting and has nothing left to wrap",
                                fp.path.display()
                            );
                            break;
                        }
                        Ok(fresh) if fresh.plan.changes == fp.plan.changes => {
                            println!(
                                "· {} changed while waiting (same servers to wrap; keeping the new content)",
                                fp.path.display()
                            );
                            fp = fresh;
                        }
                        Ok(fresh) => {
                            println!(
                                "{} changed while waiting; it would now change like this:",
                                fp.path.display()
                            );
                            print_plan(&fresh);
                            if !args.yes
                                && !confirm(&format!(
                                    "Wrap {} server(s) in {}?",
                                    fresh.plan.changes.len(),
                                    fresh.path.display()
                                ))
                            {
                                println!("Left {} unchanged.", fresh.path.display());
                                failed = true;
                                break;
                            }
                            fp = fresh;
                        }
                    }
                }
                Err(e) => {
                    println!("✗ {}: {e:#}", fp.path.display());
                    failed = true;
                    break;
                }
            }
        }
    }
    if hints.is_empty() {
        println!("\nNothing changed.");
        return Ok(i32::from(failed));
    }
    println!("\nBackups: {}", dir.dir.display());
    for c in hints {
        println!("→ {}: {}", c.label(), c.restart_hint());
    }
    println!(
        "Then use your agent as usual. `meiose stats` shows the savings; `meiose uninstall` undoes this."
    );
    Ok(i32::from(failed))
}

/// The diff for one file: each wrap as before/after lines, then what is
/// left alone and why. Never prints `env` (secrets).
fn print_plan(fp: &FilePlan) {
    for c in &fp.plan.changes {
        println!("  ~ {}", install::location_label(&c.location));
        println!("    - {}", c.before.display());
        println!("    + {}", c.after.display());
    }
    let skipped: Vec<String> = fp
        .plan
        .skipped
        .iter()
        .filter(|(_, why)| *why != Skip::Filtered)
        .map(|(loc, why)| {
            let why = match why {
                Skip::Remote => "remote server",
                Skip::AlreadyWrapped => "already through meiose",
                Skip::NoCommand => "no command",
                Skip::Filtered => unreachable!(),
            };
            format!("{} ({why})", install::location_label(loc))
        })
        .collect();
    if !skipped.is_empty() {
        println!("  unchanged: {}", skipped.join(", "));
    }
}

pub fn uninstall(args: UninstallArgs) -> anyhow::Result<i32> {
    let dir = install_dir()?;
    let mut manifest = dir.load()?;
    if manifest.files.is_empty() {
        println!("Nothing to uninstall: meiose has not changed any config files.");
        return Ok(0);
    }
    for f in &manifest.files {
        let current = std::fs::read(&f.path).ok();
        let exact =
            f.byte_restore_ok && current.is_some() && std::fs::read(&f.written).ok() == current;
        println!(
            "{} — {}\n  {}",
            f.client.label(),
            f.path.display(),
            if current.is_none() {
                "file no longer exists; nothing to do".to_string()
            } else if exact {
                "restore the original file exactly (byte-for-byte)".to_string()
            } else {
                format!(
                    "the file changed since install: unwrap the {} server(s) meiose wrapped, keep every other change",
                    f.changes.len()
                )
            }
        );
    }
    if !args.yes && !confirm("\nProceed?") {
        println!("Nothing changed. (Re-run with --yes to skip this question.)");
        return Ok(1);
    }
    let mut failed = false;
    let mut remaining = Vec::new();
    // Backups hold whole config files, secrets included, so they are deleted
    // once a file is undone (unless --keep-backups). A file that is gone, or
    // no longer has our entries, keeps its backup: it may be the only copy.
    let mut kept = 0;
    for f in std::mem::take(&mut manifest.files) {
        let undone = match install::undo_file(&f) {
            Ok(Undo::Restored) => {
                println!("✓ restored {}", f.path.display());
                true
            }
            Ok(Undo::Unwrapped(n)) => {
                println!("✓ unwrapped {n} server(s) in {}", f.path.display());
                true
            }
            Ok(Undo::NothingToUndo) => {
                println!(
                    "· {}: none of meiose's entries are there any more; left as is",
                    f.path.display()
                );
                false
            }
            Ok(Undo::Missing) => {
                println!("· {}: file is gone; skipped", f.path.display());
                false
            }
            Err(e) => {
                println!("✗ {}: {e:#}", f.path.display());
                failed = true;
                remaining.push(f);
                continue;
            }
        };
        if !undone || args.keep_backups {
            kept += 1;
            continue;
        }
        for p in [&f.backup, &f.written] {
            match std::fs::remove_file(p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    kept += 1;
                    println!(
                        "! could not delete the backup {}: {e}\n  → delete it by hand, or run `meiose purge`",
                        p.display()
                    );
                }
            }
        }
    }
    let unfinished = !remaining.is_empty();
    manifest.files = remaining;
    dir.save(&manifest)?;
    if unfinished {
        println!(
            "\nBackups are kept in {} for the file(s) that failed; fix the problem above and run `meiose uninstall` again.",
            dir.dir.display()
        );
    } else if kept > 0 {
        println!(
            "\nBackups are kept in {} in case you need them; `meiose purge` deletes them.",
            dir.dir.display()
        );
    }
    println!("Restart your agents to apply.");
    Ok(i32::from(failed))
}
