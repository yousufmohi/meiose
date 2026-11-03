# Security

## Reporting a vulnerability

Please report vulnerabilities privately through GitHub: on the
[repository page](https://github.com/yousufmohi/meiose), open **Security →
Report a vulnerability**. Don't open a public issue for anything that could
put users at risk before a fix is out.

Include the meiose version (`meiose --version`), your OS, and the steps or
input that show the problem. Fixes ship in a new release, with a note in
[CHANGELOG.md](CHANGELOG.md); tell us if you want to be credited.

Only the latest release gets security fixes.

## Threat model

meiose runs on your machine, between your agent and the MCP servers you
configured, as your user. It makes no network calls of its own.

### In scope

**Untrusted page and tool output.** Anything an MCP server returns can come
from a hostile web page or service. meiose must:

- forward it, or a trimmed version with a way back to the original, without
  corrupting the stream, hanging or crashing (it fails open: on any error the
  original goes through);
- not let it take over your terminal: `meiose log`, `log --show`, `stats` and
  `doctor` strip control characters (escape sequences) from what they print;
- not let it use unbounded memory or disk: messages over 16 MB pass through
  unprocessed, the store queue is bounded, and stored outputs are capped at
  512 MB on disk;
- not let one server read another's outputs: `meiose_expand` only returns
  outputs stored by the same server (same command line and directory), and
  expand ids carry 65 random bits.

**Secrets at rest.** Tool outputs, logs, config backups and recordings can
contain credentials. meiose must:

- keep them readable by you only on macOS and Linux (directories `700`,
  files `600`, set at creation) and keep the permissions of the config files
  it edits;
- keep them no longer than needed: outputs expire after 24 hours
  (`MEIOSE_KEEP_HOURS`) and are deleted the next time meiose runs, overwritten
  in the database rather than just unlinked; a server run with `--no-store`
  never has its outputs written to disk; `meiose uninstall` deletes its
  backups; `meiose purge` deletes everything else;
- not copy server arguments (which often carry API keys) into logs, stats or
  file names, and hide secret-looking values where it shows a command line.

### Out of scope

- **Malware or another program running as your user.** It can read your
  configs, your agent's files and meiose's data directly, and can change the
  meiose binary. File permissions do not protect against your own account.
- **Someone who can edit your agent's MCP config.** They choose which
  commands run; meiose runs what the config says.
- **The MCP servers themselves**, and what your agent does with their output
  (prompt injection in page content is passed through like any other text).
- **Windows file permissions.** meiose's files get the default permissions
  of your profile folder, which other standard users cannot read.
- **`meiose record`**, a debugging tool: recordings contain all traffic,
  including anything typed into forms, by design.

## Release integrity

Releases are built by GitHub Actions from a `v*` tag with
[dist](https://github.com/axodotdev/cargo-dist). Every action is pinned to a
full commit SHA, and, from the first release after 0.2.2, every release
archive has a GitHub artifact attestation, which you can check with:

```bash
gh attestation verify <downloaded archive> --repo yousufmohi/meiose
```

crates.io releases are published by hand from the tagged commit.
