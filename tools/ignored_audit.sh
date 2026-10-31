#!/usr/bin/env bash
# Every #[ignore]d test of this repository must be run by a step of the gate: a test nobody runs is a test nobody notices
# failing. Fails and names each file that holds an ignored test no step claims.
#
#   tools/ignored_audit.sh
set -euo pipefail
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
cd "$here"
bad=0
while IFS= read -r file; do
    case "$file" in
        crates/radovid/tests/docker.rs) ;;   # gate step docker-test: with DOCKER_TEST_IMAGE
        *) echo "ignored_audit: $file has ignored tests that no gate step runs"; bad=1 ;;
    esac
done < <(git ls-files '*.rs' | xargs grep -l '#\[ignore')
[ "$bad" = 0 ] && echo "ignored_audit: every ignored test has a step"
exit "$bad"
