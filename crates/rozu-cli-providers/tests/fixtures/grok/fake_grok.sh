#!/bin/sh
# A fake Grok Build CLI for rozu's tests. `grok … agent … stdio` follows a script of ACP
# (JSON-RPC) steps; it never talks to xAI and knows nothing about credentials.
#
#   FAKE_GROK_SCRIPT  the agent script, one step per line:
#                       < METHOD   read one line from rozu (logged), which must be a message with
#                                  that method; a request's numeric id becomes __ID__ (a line
#                                  without one, e.g. a notification or a reply, keeps the last id)
#                       <          read one line from rozu (logged), e.g. a permission answer
#                       > JSON     print JSON, with __ID__ replaced (and __CWD__ by the agent's
#                                  working directory, the workspace)
#                       stderr T   print T on stderr
#                       sleep N / exit N
#                     After the script, every further line from rozu is logged until stdin ends.
#   FAKE_GROK_LOG     directory for what rozu sent: args.txt (argv of every agent start,
#                     separated by `--`), stdin.jsonl, env.txt, cwd.txt, profile.md (the agent
#                     profile rozu passed), login_args.txt

log=${FAKE_GROK_LOG:-/dev/null}

case "$1" in
--version)
    echo "grok 1.0.52 (fake)"
    exit 0
    ;;
login)
    printf '%s\n' "$@" >"$log/login_args.txt"
    echo "fake grok: login finished"
    exit 0
    ;;
esac

# Everything else must be `--no-auto-update agent … stdio`.
{
    for arg in "$@"; do printf '%s\n' "$arg"; done
    echo "--"
} >>"$log/args.txt"
last=
profile=
prev=
for arg in "$@"; do
    [ "$prev" = --agent-profile ] && profile=$arg
    prev=$arg
    last=$arg
done
if [ "$last" != stdio ]; then
    echo "fake grok: expected stdio, got $*" >&2
    exit 2
fi
[ -n "$profile" ] && cp "$profile" "$log/profile.md"
{
    echo "XAI_API_KEY=${XAI_API_KEY:-unset}"
    echo "GROK_CLI_CHAT_PROXY_BASE_URL=${GROK_CLI_CHAT_PROXY_BASE_URL:-unset}"
} >"$log/env.txt"
cwd=$(pwd)
echo "$cwd" >"$log/cwd.txt"
echo "fake grok: agent starting (stderr noise for the log file)" >&2

exec 3<"$FAKE_GROK_SCRIPT"
id=
while IFS= read -r step <&3 || [ -n "$step" ]; do
    case "$step" in
    '#'* | '') ;;
    '<'*)
        want=${step#<}
        want=${want# }
        IFS= read -r line || exit 0
        printf '%s\n' "$line" >>"$log/stdin.jsonl"
        if [ -n "$want" ]; then
            case "$line" in
            *"\"method\":\"$want\""*) ;;
            *)
                echo "fake grok: expected $want, got $line" >&2
                exit 3
                ;;
            esac
        fi
        got=$(printf '%s\n' "$line" | sed -n 's/^{\("[a-z]*":"[^"]*",\)*"id":\([0-9][0-9]*\).*/\2/p')
        [ -n "$got" ] && id=$got
        ;;
    '> '*) printf '%s\n' "${step#> }" | sed "s/__ID__/$id/g; s|__CWD__|$cwd|g" ;;
    'stderr '*) echo "${step#stderr }" >&2 ;;
    'sleep '*) sleep "${step#sleep }" ;;
    'exit '*) exit "${step#exit }" ;;
    *)
        echo "fake grok: bad script step: $step" >&2
        exit 4
        ;;
    esac
done
while IFS= read -r line; do printf '%s\n' "$line" >>"$log/stdin.jsonl"; done
