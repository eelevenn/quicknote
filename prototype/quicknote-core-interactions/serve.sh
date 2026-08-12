#!/usr/bin/env bash
# 启动无依赖的本地静态服务器，便于直接体验 throwaway prototype。
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
python3 -m http.server 4173 --directory "$SCRIPT_DIR"
