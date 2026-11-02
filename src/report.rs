//! Human-readable output for `meiose stats` and `meiose log`.

use std::fmt::Write as _;

use serde_json::Value;

use crate::store::{CallRow, ToolTotals, Totals};

/// 2_100_000 → "2.1M", 340_000 → "340K", 950 → "950".
pub fn short(n: i64) -> String {
    let f = n as f64;
    if n.abs() >= 1_000_000 {
        format!("{:.1}M", f / 1e6)
    } else if n.abs() >= 10_000 {
        format!("{:.0}K", f / 1e3)
    } else if n.abs() >= 1_000 {
        format!("{:.1}K", f / 1e3)
    } else {
        n.to_string()
    }
}

fn pct_saved(before: i64, after: i64) -> f64 {
    if before <= 0 {
        0.0
    } else {
        100.0 * (1.0 - after as f64 / before as f64)
    }
}

pub fn stats(days: u32, t: &Totals, per_tool: &[ToolTotals]) -> String {
    let mut s = String::new();
    if t.calls == 0 {
        let _ = writeln!(
            s,
            "No tool calls recorded in the last {days} days.\n  \
             → Run an MCP server through meiose (`meiose run -- <server command>`) and use it; \
             calls show up here as they happen."
        );
        return s;
    }
    // Expanded outputs are tokens the agent paid for after all: count them.
    let net_after = t.tokens_after + t.expand_tokens;
    let _ = writeln!(
        s,
        "Last {days} days: {} tokens → {} ({:.0}% saved)",
        short(t.tokens_before),
        short(net_after),
        pct_saved(t.tokens_before, net_after)
    );
    let _ = writeln!(
        s,
        "  {} tool calls, {} trimmed · {} expands ({} tokens, included above)",
        t.calls,
        t.trimmed_calls,
        t.expands,
        short(t.expand_tokens)
    );
    if !per_tool.is_empty() {
        let _ = writeln!(s);
        let w_srv = per_tool
            .iter()
            .map(|r| r.server.len())
            .max()
            .unwrap_or(6)
            .max(6);
        let w_tool = per_tool
            .iter()
            .map(|r| r.tool.len())
            .max()
            .unwrap_or(4)
            .max(4);
        let _ = writeln!(
            s,
            "  {:<w_srv$}  {:<w_tool$}  {:>6}  {:>8}  {:>8}  {:>6}",
            "server", "tool", "calls", "before", "after", "saved"
        );
        for r in per_tool.iter().take(15) {
            let _ = writeln!(
                s,
                "  {:<w_srv$}  {:<w_tool$}  {:>6}  {:>8}  {:>8}  {:>5.0}%",
                r.server,
                r.tool,
                r.calls,
                short(r.tokens_before),
                short(r.tokens_after),
                pct_saved(r.tokens_before, r.tokens_after)
            );
        }
    }
    let _ = writeln!(
        s,
        "\n  Token counts are estimates (within about 10%); set MEIOSE_EXACT_TOKENS=1 \
         on the proxy for exact counts."
    );
    s
}

pub fn ago(now_ms: i64, ts_ms: i64) -> String {
    let secs = ((now_ms - ts_ms) / 1000).max(0);
    match secs {
        0..=59 => format!("{secs}s ago"),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

pub fn log(now_ms: i64, rows: &[CallRow]) -> String {
    let mut s = String::new();
    if rows.is_empty() {
        let _ = writeln!(
            s,
            "No tool calls recorded yet.\n  → Run an MCP server through meiose (`meiose run -- <server command>`) and use it."
        );
        return s;
    }
    for r in rows {
        let change = if r.kind == "expand" {
            format!("{:>8} tok returned", short(r.tokens_after))
        } else if r.handler.is_some() {
            format!(
                "{:>7} → {:>6} tok {:>4.0}%",
                short(r.tokens_before),
                short(r.tokens_after),
                -pct_saved(r.tokens_before, r.tokens_after)
            )
        } else {
            format!("{:>7} tok (untouched)", short(r.tokens_before))
        };
        let _ = writeln!(
            s,
            "{:>8}  {:<20}  {:<22}  {change:<28}  {:<16}  {}",
            ago(now_ms, r.ts_ms),
            truncate(&r.server, 20),
            truncate(&r.tool, 22),
            r.handler
                .as_deref()
                .unwrap_or(if r.kind == "expand" { "expand" } else { "-" }),
            r.expand_id
                .as_deref()
                .map(|i| format!("id={i}"))
                .unwrap_or_default(),
        );
    }
    let _ = writeln!(
        s,
        "\n  → `meiose log --show <id>` prints the trimmed and original output."
    );
    s
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n - 1).collect();
        t.push('…');
        t
    }
}

/// The text content of a stored result JSON, for `log --show`.
pub fn result_text(result_json: &str) -> String {
    match serde_json::from_str::<Value>(result_json) {
        Ok(v) => v
            .get("content")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|i| match i.get("text").and_then(Value::as_str) {
                        Some(t) => t.to_string(),
                        None => format!(
                            "<{} content>",
                            i.get("type").and_then(Value::as_str).unwrap_or("?")
                        ),
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_else(|| result_json.to_string()),
        Err(_) => result_json.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_numbers() {
        assert_eq!(short(2_100_000), "2.1M");
        assert_eq!(short(340_000), "340K");
        assert_eq!(short(1_500), "1.5K");
        assert_eq!(short(950), "950");
    }

    #[test]
    fn stats_headline_counts_expands_against_savings() {
        let t = Totals {
            calls: 10,
            trimmed_calls: 8,
            tokens_before: 2_100_000,
            tokens_after: 330_000,
            expands: 2,
            expand_tokens: 10_000,
        };
        let out = stats(7, &t, &[]);
        assert!(
            out.starts_with("Last 7 days: 2.1M tokens → 340K (84% saved)"),
            "{out}"
        );
        assert!(out.contains("2 expands"));
    }

    #[test]
    fn empty_stats_say_what_to_do() {
        let out = stats(7, &Totals::default(), &[]);
        assert!(out.contains("→ Run an MCP server through meiose"));
    }

    #[test]
    fn ago_formats() {
        assert_eq!(ago(100_000, 95_000), "5s ago");
        assert_eq!(ago(10_000_000, 9_880_000), "2m ago");
    }
}
