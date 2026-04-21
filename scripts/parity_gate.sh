#!/usr/bin/env bash
# PORT-PLAN §4.1.4 — thin wrapper over the parity-binary harness.
# The parity binary (libtest-mimic, harness = false) emits its gate report
# and exit code unconditionally; this script just forwards.

set -euo pipefail

cargo test -p omsrs --test parity --release --all-features --no-run
exec cargo test -p omsrs --test parity --release --all-features
