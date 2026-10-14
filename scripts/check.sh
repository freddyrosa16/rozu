#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

usage() {
    echo "Usage: ./scripts/check.sh [all|quick|rust|security]"
    echo "  all       Run every check (default; requires Node.js and cargo-deny)."
    echo "  quick     Check Rust formatting and test the landing page."
    echo "  rust      Run Clippy, Rust tests, and the bundled mocked evaluations."
    echo "  security  Check dependency advisories, licenses, and allowed sources."
}

quick() {
    cargo fmt --all --check
    node --test tests/waves.test.cjs
}

rust_checks() {
    cargo clippy --locked --workspace --all-targets -- -D warnings
    cargo test --locked --workspace

    check_eval_dir="$(mktemp -d "${TMPDIR:-/tmp}/rozu-evals.XXXXXX")"
    trap 'rm -f "$check_eval_dir/memory.jsonl"; rmdir "$check_eval_dir"' EXIT
    # These are reviewed repository fixtures. Imported evals can execute commands too.
    cargo run --locked -q -p rozu-cli -- eval tasks --trusted
    cargo run --locked -q -p rozu-cli -- eval replay --trusted \
        evals/recorded/fix-failing-test.jsonl --workspace evals/recorded/fix-failing-test
    cargo run --locked -q -p rozu-cli -- eval memory --out "$check_eval_dir/memory.jsonl"
}

security() {
    cargo deny --all-features --locked check
}

if [[ $# -gt 1 ]]; then
    usage >&2
    exit 2
fi

case "${1:-all}" in
    all) quick; rust_checks; security ;;
    quick) quick ;;
    rust) rust_checks ;;
    security) security ;;
    -h|--help) usage ;;
    *) usage >&2; exit 2 ;;
esac
