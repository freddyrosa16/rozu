#!/bin/sh
# A fake `claude` CLI for rozu's tests: replays recorded stream-json transcripts. It never talks
# to Anthropic and knows nothing about credentials.
#
#   FAKE_CLAUDE_TURNS  directory with the scenario:
#                        turnN.jsonl   stdout for the Nth user message (counted across restarts)
#                        hangN         after turnN.jsonl, hang instead of waiting for input
#                        mcpN.jsonl    before turnN.jsonl, pipe these MCP requests through the
#                                      server named in --mcp-config (with its env)
#                        stderr.txt    after reading the first message, print this to stderr
#                                      and exit 1
#                        auth.json     output of `claude auth status`
#   FAKE_CLAUDE_LOG    directory for what rozu sent: args.txt (argv of every start, separated
#                      by `--`), stdin.jsonl, env.txt, mcpN.out

case "$1" in
--version)
    echo "9.9.9 (Claude Code)"
    exit 0
    ;;
auth)
    cat "$FAKE_CLAUDE_TURNS/auth.json"
    exit 0
    ;;
esac

log=$FAKE_CLAUDE_LOG
{
    for arg in "$@"; do printf '%s\n' "$arg"; done
    echo "--"
} >>"$log/args.txt"
echo "ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY:-unset}" >"$log/env.txt"
echo "fake claude: starting up (stderr noise for the log file)" >&2

config=
while [ $# -gt 0 ]; do
    if [ "$1" = "--mcp-config" ]; then config=$2; fi
    shift
done

# Runs the MCP server from the config: `command`, the `args` array and ROZU_APPROVAL_SOCKET
# (serde_json writes one array element per line).
run_mcp() {
    requests=$1
    socket=$(sed -n 's/^ *"ROZU_APPROVAL_SOCKET": "\(.*\)"$/\1/p' "$config")
    command=$(sed -n 's/^ *"command": "\(.*\)",$/\1/p' "$config")
    set --
    in_args=0
    while IFS= read -r l; do
        case "$l" in
        *'"args": ['*)
            in_args=1
            continue
            ;;
        esac
        if [ "$in_args" = 1 ]; then
            case "$l" in
            *']'*)
                in_args=0
                continue
                ;;
            esac
            set -- "$@" "$(printf '%s' "$l" | sed 's/^ *"//; s/",\{0,1\}$//')"
        fi
    done <"$config"
    ROZU_APPROVAL_SOCKET=$socket "$command" "$@" <"$requests" >"$log/mcp$n.out"
}

while IFS= read -r line; do
    printf '%s\n' "$line" >>"$log/stdin.jsonl"
    n=$(($(cat "$log/count" 2>/dev/null || echo 0) + 1))
    echo "$n" >"$log/count"
    if [ -f "$FAKE_CLAUDE_TURNS/stderr.txt" ]; then
        cat "$FAKE_CLAUDE_TURNS/stderr.txt" >&2
        exit 1
    fi
    if [ -f "$FAKE_CLAUDE_TURNS/mcp$n.jsonl" ]; then
        run_mcp "$FAKE_CLAUDE_TURNS/mcp$n.jsonl"
    fi
    if [ ! -f "$FAKE_CLAUDE_TURNS/turn$n.jsonl" ]; then
        echo "fake claude: no transcript for turn $n" >&2
        exit 3
    fi
    cat "$FAKE_CLAUDE_TURNS/turn$n.jsonl"
    if [ -f "$FAKE_CLAUDE_TURNS/hang$n" ]; then
        sleep 30
    fi
done
