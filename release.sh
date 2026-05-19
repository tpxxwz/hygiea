#!/usr/bin/env bash
set -euo pipefail

# 用法：
#   ./release.sh patch              # dry-run
#   ./release.sh patch --execute    # 正式发布
#   ./release.sh 0.1.1-alpha.2 --execute

BUMP="${1:-patch}"

if [ "${2:-}" = "--execute" ]; then
    cargo release "$BUMP" --workspace --config release/release.toml --execute
else
    echo ">> Dry-run 模式，确认无误后加 --execute"
    cargo release "$BUMP" --workspace --config release/release.toml
fi
