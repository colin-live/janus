#!/usr/bin/env bash
# Build and run the C ABI smoke test on Unix (CI counterpart of run_c_smoke.ps1).
#
#   ./tests/run_c_smoke.sh [steps] [sim_id_hex]
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
workspace=$(cd "$root/../.." && pwd)
steps=${1:-5}
sim_hex=${2:-$(od -An -N8 -tx8 /dev/urandom | tr -d ' \n')}
debug="$workspace/target/debug"

cd "$workspace"
cargo build -p coupling-client
cargo build -p coupling-client --example mock_host

cc -std=c11 -Wall -Wextra -Werror \
    -I "$root/include" \
    "$root/tests/c_smoke.c" \
    "$debug/libcoupling_client.so" \
    -o "$debug/c_smoke"

"$debug/examples/mock_host" "0x$sim_hex" 1 "$steps" 15000 \
    >"$debug/c_smoke_host.out" 2>"$debug/c_smoke_host.err" &
host_pid=$!
trap 'kill "$host_pid" 2>/dev/null || true' EXIT

LD_LIBRARY_PATH="$debug${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
    "$debug/c_smoke" "0x$sim_hex" "$steps"

wait "$host_pid"

if ! grep -q HOST_DONE "$debug/c_smoke_host.out"; then
    echo "mock_host did not report HOST_DONE" >&2
    cat "$debug/c_smoke_host.err" >&2
    exit 1
fi

echo "C ABI smoke test passed ($steps steps, sim_id=0x$sim_hex)"
