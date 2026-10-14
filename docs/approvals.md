# Approvals and the sandbox

Every tool call gets a risk class (read, write in workspace, write outside, network, opaque, destructive) and is
either auto-approved or shown as an approval card with the exact command or diff, the sandbox and the network state:

| `--approval` | runs without asking | asks |
|---|---|---|
| `read-only` | reads | everything else |
| `auto` (default) | reads, sandboxed workspace writes | network, writes outside the workspace, opaque, destructive |
| `full` | everything else | opaque, destructive |

The desktop app offers the same three modes on its setup page (the gear at the bottom left); the choice applies
to chats started from then on.

## The sandbox is the boundary
**In every mode, `full` included.** On macOS, shell commands run under Seatbelt (`sandbox-exec`): they can read
anywhere but only write inside the workspace, a private per-session temp dir (the command's `$TMPDIR`; `/tmp` and
your own `$TMPDIR` are not writable) and `/dev`, and the network is blocked unless that call was approved with
network access (the agent sets `network: true`, or the command is recognized as networked, e.g. `curl`,
`git fetch`, `npm install`). When you approve a command that writes to named paths outside the workspace
(`cp build.tgz ~/Downloads/`), the card lists them and the sandbox lets that one command write exactly those
paths. If the sandbox blocks something anyway, the output says `blocked by rozu sandbox: writes outside
workspace…` and rozu asks once whether to rerun it unsandboxed.

**Tool caches outside the workspace are blocked too, in every mode.** Writes to `~/.cargo`, `~/.npm`,
`~/.cache/pip`, `~/go`, `~/Library/Caches/…` and the like fail under the sandbox even in `--approval full`: `cargo`
locks `~/.cargo/.package-cache` on every build, `npm install` writes `~/.npm`. You'll see the `blocked by rozu
sandbox` note and a one-time offer to rerun that command with `u` (unsandboxed, this command only; nothing is
remembered). To avoid the prompt, keep the caches where the sandbox allows writes, by starting rozu with them in the
workspace (add the directories to `.gitignore`):

```sh
CARGO_HOME="$PWD/.cache/cargo" npm_config_cache="$PWD/.cache/npm" PIP_CACHE_DIR="$PWD/.cache/pip" \
  GOMODCACHE="$PWD/.cache/go/mod" GOCACHE="$PWD/.cache/go/build" rozu chat
```

or per command in the session temp dir, e.g. `CARGO_HOME="$TMPDIR/cargo" cargo build` or `npm install --cache
"$TMPDIR/npm"` (deleted when rozu exits). A fresh cache has to be filled once, which needs network access, so that
first command asks for approval.

There is no sandbox on Linux (or anywhere but macOS) yet: `rozu chat` and `rozu doctor` print a red warning banner
there, in auto mode shell commands that write ask first, and in full mode they run unconfined without asking.
Commands time out after 120 s by default (max 600 s), and the whole process group is killed on timeout or Ctrl-C.

## What always asks
Destructive commands (`rm -rf`, `git push --force`, `git reset --hard`, `git branch -D`, `git clean -fd`, `sudo`,
redirects to paths outside the workspace, …) **always** ask, in every mode. So do **opaque** commands, whose effect
rozu can't read off the command line: inline code (`python -c`, `perl -e`, `node -e`, `sh -c`/`bash -c`/`zsh -c`,
code on an interpreter's stdin or in a heredoc, `eval`, `source`, `exec`), other interpreters (`osascript`, …),
scripts outside the workspace, Git commands (repository configuration can execute programs), unrecognized
programs/options, shell assignments/wrappers, execution-capable network clients (`ssh`, `scp`, `npx`,
`npm exec`, and similar), and any output redirect or `tee` to a file (`> out.txt`). A **script file inside the
workspace** is treated the same whichever interpreter runs it (`sh`, `bash`, `zsh`, `python`, `perl`, `ruby`,
`node`): like `make`, it's a sandboxed workspace write, so `sh test.sh` and `python test.py` run without a prompt in
`auto` and `full`.

## Answering a prompt
Keys at the CLI prompt (the app shows the same choices as buttons):

| key | does |
|---|---|
| `y` | approve; the command still runs in the sandbox |
| `n` | deny; `n <reason>` (or a reason on the next line) is passed to the agent |
| `a` | approve and always allow this rule in this project: the command prefix (`cargo test`), the interpreter and workspace script (`python3 tests.py`). Not offered for network, opaque or destructive commands |
| `u` | run **unsandboxed, this command only**: no sandbox at all, full access to your files and the network. Never remembered, never chosen by a mode or rule. Only offered where there is a sandbox to lift |

Rules live in the user's configuration directory under `approvals/<workspace-sha256>.json`, keyed by the
canonical workspace path. The directory is private (0700), and files are private (0600). Repository
`.rozu/approvals.json` files are ignored: old approvals must be made again. If the configuration directory
itself lies inside the open workspace, rules stay in memory for that session. Network permissions are never
remembered because the sandbox cannot enforce destination-host restrictions; legacy host rules are inert.
Without a terminal on stdin, every prompt is denied.

Cards escape terminal and bidirectional control characters, show the complete available preview and list every
additional writable path. The desktop preview scrolls in both directions. Approvals show complete command,
content, diff and permission details; prompts above 2 MiB are denied and must be split into smaller calls.
Linux cards say there is no OS sandbox.

## Audit log
Every approval decision (automatic or yours, allowed or denied) is appended to the audit log
`~/Library/Logs/rozu/approvals.jsonl` (owner-only; `approvals.jsonl` in the data directory on other systems;
`ROZU_AUDIT_LOG` moves it): time, session id, tool, decision and who made it, rule, mode, risk, sandboxed or not,
granted paths, a SHA-256 of the command or diff and a redacted, shortened copy of it. Decisions made for a vendor
CLI carry its `"provider"`.
