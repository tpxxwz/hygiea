#!/usr/bin/env bash
set -euo pipefail
cargo audit --ignore RUSTSEC-2023-0071
cargo test --workspace
