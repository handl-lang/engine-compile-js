#!/bin/sh
# P-002/T-017 bench runner — compiles bench/workloads/*.hnl under every
# strategy variant, executes under node, writes bench/results.json.
#
#   ./bench/run.sh            # full run (BENCH_N=30 timed iters)
#   ./bench/run.sh --quick    # smoke pass (fewer iters)
#   BENCH_N=10 ./bench/run.sh # env-tunable iteration counts
set -eu
cd "$(dirname "$0")/.."
exec cargo run --offline --example bench -- "$@"
