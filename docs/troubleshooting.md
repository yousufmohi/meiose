# Troubleshooting

Start with `meiose doctor`. It checks the binary, the database and every server
entry in every config, and prints a `→` fix for each problem.

## A server stopped starting after install

Run `meiose doctor`. If it says the server's command is not on PATH, use the
command's full path in the config. Your agent may start servers with a
different PATH than your terminal.

## The agent misses something on a page

It should call `meiose_expand`; smaller models sometimes don't. Try `--no-diff`,
or `--passthrough` to compare, and open an issue with the output of
`meiose log --show <id>`.

## `meiose install` refuses: "running from a temporary location"

You ran it through `npx` or from a build folder. Install meiose permanently
first, or pass `--meiose-path`.

## Windows: "Access is denied" when updating meiose

A running agent is using the binary. Close those sessions and try again.
