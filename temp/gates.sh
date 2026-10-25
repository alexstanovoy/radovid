set -u
run() { echo "### $*"; "$@" 2>&1 | tail -${TAILN:-6}; echo "rc=${PIPESTATUS[0]}"; }
run cargo fmt --all --check
run cargo clippy --all-targets -- -D warnings
run cargo test --release
run cargo deny check
