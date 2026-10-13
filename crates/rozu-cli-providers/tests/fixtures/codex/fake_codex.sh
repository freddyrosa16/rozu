#!/bin/sh
# A fake `codex` CLI for rozu's tests. `codex app-server` follows a script of JSON-RPC steps; it
# never talks to OpenAI and knows nothing about credentials.
#
#   FAKE_CODEX_SCRIPT  the app-server script, one step per line:
#                        < METHOD   read one line from rozu (logged), which must be a message with
#                                   that method; a request's numeric id becomes __ID__
#                        <          read one line from rozu (logged), e.g. an approval answer
#                        > JSON     print JSON, with __ID__ replaced (and __CWD__ by the
#                                   app-server's working directory, the workspace)
#                        stderr T   print T on stderr
#                        sleep N / exit N
#                      After the script, every further line from rozu is logged until stdin ends.
#   FAKE_CODEX_LOG     directory for what rozu sent: args.txt (argv of every app-server start,
#                      separated by `--`), stdin.jsonl, env.txt, cwd.txt, login_args.txt
#   FAKE_CODEX_LOGIN_STATUS  what `codex login status` prints (on stderr, like Codex)

log=${FAKE_CODEX_LOG:-/dev/null}

case "$1" in
--version)
    echo "codex-cli 0.147.0"
    exit 0
    ;;
login)
    if [ "$2" = status ]; then
        printf '%s\n' "${FAKE_CODEX_LOGIN_STATUS:-Not logged in}" >&2
        case "$FAKE_CODEX_LOGIN_STATUS" in "Logged in"*) exit 0 ;; esac
        exit 1
    fi
    printf '%s\n' "$@" >"$log/login_args.txt"
    echo "fake codex: login finished"
    exit 0
    ;;
esac

# Everything else must be `[-c key=value]… app-server`.
{
    for arg in "$@"; do printf '%s\n' "$arg"; done
    echo "--"
} >>"$log/args.txt"
last=
for arg in "$@"; do last=$arg; done
if [ "$last" != app-server ]; then
    echo "fake codex: expected app-server, got $*" >&2
    exit 2
fi
echo "OPENAI_API_KEY=${OPENAI_API_KEY:-unset}" >"$log/env.txt"
cwd=$(pwd)
echo "$cwd" >"$log/cwd.txt"
echo "fake codex: app-server starting (stderr noise for the log file)" >&2

exec 3<"$FAKE_CODEX_SCRIPT"
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
                echo "fake codex: expected $want, got $line" >&2
                exit 3
                ;;
            esac
        fi
        id=$(printf '%s\n' "$line" | sed -n 's/^{"id":\([0-9][0-9]*\),.*/\1/p')
        ;;
    '> '*) printf '%s\n' "${step#> }" | sed "s/__ID__/$id/g; s|__CWD__|$cwd|g" ;;
    'stderr '*) echo "${step#stderr }" >&2 ;;
    'sleep '*) sleep "${step#sleep }" ;;
    'exit '*) exit "${step#exit }" ;;
    *)
        echo "fake codex: bad script step: $step" >&2
        exit 4
        ;;
    esac
done
while IFS= read -r line; do printf '%s\n' "$line" >>"$log/stdin.jsonl"; done
