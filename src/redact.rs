//! Hide secrets in server command lines before they are shown or stored.
//!
//! Servers often take credentials as arguments (`--api-key sk-…`,
//! `postgres://user:password@host/db`, `-e GITHUB_TOKEN=…`). meiose never
//! logs a server's arguments; where it must show a command line (the install
//! diff, a recording's header) the values below are replaced with `***`:
//! - the value after a flag whose name mentions a secret (`--api-key X`);
//! - `--flag=value` and `NAME=value` with such a name;
//! - the password in a URL (`scheme://user:***@host`) and secret-named
//!   query parameters;
//! - anything that looks like a bare token (long, letters and many digits,
//!   no path or package punctuation).
//!
//! This is a best effort for display, not a guarantee: an unusual secret
//! can slip through, so meiose also never puts arguments in its logs.

pub const MASK: &str = "***";

const SECRET_WORDS: &[&str] = &[
    "key",
    "token",
    "secret",
    "pass",
    "pwd",
    "auth",
    "credential",
    "bearer",
    "cookie",
    "session",
    "dsn",
    "private",
    "signature",
    "header",
];

/// Does a flag, variable or parameter name suggest its value is a secret?
pub fn is_secret_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    SECRET_WORDS.iter().any(|w| n.contains(w))
}

/// `args` with secret values replaced by [`MASK`]. The same length as `args`.
pub fn args(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut mask_next = false;
    for a in args {
        if std::mem::take(&mut mask_next) && !a.starts_with('-') {
            out.push(MASK.to_string());
            continue;
        }
        if a.starts_with('-') && a.len() > 1 {
            let name = a.trim_start_matches('-');
            match name.split_once('=') {
                Some((n, v)) => {
                    let flag = &a[..a.len() - v.len()];
                    if is_secret_name(n) {
                        out.push(format!("{flag}{MASK}"));
                    } else {
                        out.push(format!("{flag}{}", value(v)));
                    }
                }
                None => {
                    mask_next = is_secret_name(name);
                    out.push(a.clone());
                }
            }
            continue;
        }
        out.push(value(a));
    }
    out
}

/// One argument that is not a flag.
pub fn value(a: &str) -> String {
    if a.contains("://") {
        return url(a);
    }
    if let Some((name, _)) = a.split_once('=')
        && !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        && is_secret_name(name)
    {
        return format!("{name}={MASK}");
    }
    if looks_like_token(a) {
        return MASK.to_string();
    }
    a.to_string()
}

/// `postgres://u:pw@h/db?sslpassword=x&ssl=1` → `postgres://u:***@h/db?sslpassword=***&ssl=1`.
fn url(a: &str) -> String {
    let Some(scheme_end) = a.find("://") else {
        return a.to_string();
    };
    let (scheme, rest) = a.split_at(scheme_end + 3);
    let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(auth_end);
    let authority = match authority.rsplit_once('@') {
        Some((userinfo, host)) => match userinfo.split_once(':') {
            Some((user, _)) => format!("{user}:{MASK}@{host}"),
            // A lone userinfo is usually a token (`https://TOKEN@host`).
            None => format!("{MASK}@{host}"),
        },
        None => authority.to_string(),
    };
    let tail = match tail.split_once('?') {
        Some((path, query)) => {
            let (query, frag) = match query.split_once('#') {
                Some((q, f)) => (q, Some(f)),
                None => (query, None),
            };
            let q: Vec<String> = query
                .split('&')
                .map(|kv| match kv.split_once('=') {
                    Some((k, _)) if is_secret_name(k) => format!("{k}={MASK}"),
                    _ => kv.to_string(),
                })
                .collect();
            let mut t = format!("{path}?{}", q.join("&"));
            if let Some(f) = frag {
                t.push('#');
                t.push_str(f);
            }
            t
        }
        None => tail.to_string(),
    };
    format!("{scheme}{authority}{tail}")
}

/// `sk-live-0123456789abcdef`, `ghp_…`: long, mixed, no path punctuation.
fn looks_like_token(a: &str) -> bool {
    a.len() >= 20
        && !a.contains(['/', '\\', '.', '@', ' '])
        && a.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '='))
        && a.chars().any(|c| c.is_ascii_alphabetic())
        && a.chars().filter(char::is_ascii_digit).count() >= 4
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(a: &[&str]) -> Vec<String> {
        args(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn flag_values_with_secret_names_are_hidden() {
        assert_eq!(
            r(&[
                "--api-key",
                "abc",
                "--port",
                "80",
                "--token=xyz",
                "--header",
                "Authorization: Bearer q"
            ]),
            [
                "--api-key",
                "***",
                "--port",
                "80",
                "--token=***",
                "--header",
                "***"
            ]
        );
        // A flag followed by another flag hides nothing.
        assert_eq!(r(&["--password", "--verbose"]), ["--password", "--verbose"]);
        assert_eq!(
            r(&["-y", "@playwright/mcp@latest"]),
            ["-y", "@playwright/mcp@latest"]
        );
    }

    #[test]
    fn urls_lose_their_passwords_and_secret_parameters() {
        assert_eq!(
            value("postgres://admin:hunter2@db.internal:5432/shop"),
            "postgres://admin:***@db.internal:5432/shop"
        );
        assert_eq!(
            value("https://x.example/mcp?api_key=s&page=2#top"),
            "https://x.example/mcp?api_key=***&page=2#top"
        );
        assert_eq!(
            value("https://tok123@github.com/o/r"),
            "https://***@github.com/o/r"
        );
        assert_eq!(
            value("https://docs.example/mcp"),
            "https://docs.example/mcp"
        );
        assert_eq!(
            r(&["--db=postgres://u:p@h/x"]),
            ["--db=postgres://u:***@h/x"]
        );
    }

    #[test]
    fn env_style_assignments_and_bare_tokens_are_hidden() {
        assert_eq!(
            r(&[
                "run",
                "-e",
                "GITHUB_TOKEN=ghp_x",
                "-e",
                "LOG=debug",
                "sk-live-0123456789abcdef"
            ]),
            ["run", "-e", "GITHUB_TOKEN=***", "-e", "LOG=debug", "***"]
        );
        for keep in [
            "@modelcontextprotocol/server-filesystem",
            "ghcr.io/github/github-mcp-server",
            "C:\\Users\\me\\Desktop",
            "server-filesystem-extra-long",
            "postgres-mcp",
        ] {
            assert_eq!(value(keep), keep);
        }
    }
}
