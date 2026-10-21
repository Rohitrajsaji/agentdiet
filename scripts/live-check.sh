#!/bin/sh
# Live check: do the assistant CLIs installed on this machine accept what `agentdiet init` writes?
#
# Everything happens in a throwaway HOME (mktemp); your real HOME, logins and configs are never read or
# written. Only commands that need no login are used:
#   codex  mcp list / mcp get   parses ~/.codex/config.toml          (CODEX_HOME points at the temp dir)
#   claude mcp list             starts every MCP server and checks it ("Connected")
#   gemini mcp list             starts every MCP server and checks it
# Cursor, Windsurf and VS Code Copilot have no such CLI command; check them in the app (see README).
#
# usage: scripts/live-check.sh [path/to/agentdiet]     (default: target/release/agentdiet)
set -u
bin=${1:-target/release/agentdiet}
case $bin in /*) ;; *) bin=$(pwd)/$bin ;; esac
[ -x "$bin" ] || { echo "no agentdiet binary at $bin (build it: cargo build --release --locked)" >&2; exit 2; }

tmp=$(mktemp -d "${TMPDIR:-/tmp}/agentdiet-live.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM
mkdir -p "$tmp/home/.codex" "$tmp/home/.claude" "$tmp/home/.gemini" "$tmp/work"
printf 'model = "o3"\n' > "$tmp/home/.codex/config.toml"
printf '{}\n' > "$tmp/home/.claude/settings.json"
printf '{}\n' > "$tmp/home/.gemini/settings.json"
cd "$tmp/work" || exit 2
HOME="$tmp/home" "$bin" init --only claude,codex,gemini > /dev/null || { echo "agentdiet init failed" >&2; exit 1; }

failed=0
report() { printf '%-8s %-12s %s\n' "$1" "$2" "$3"; }
if command -v codex > /dev/null 2>&1; then
  if HOME="$tmp/home" CODEX_HOME="$tmp/home/.codex" codex mcp get agentdiet --json 2> /dev/null | grep -q '"type": "stdio"'; then
    report codex parsed "$(codex --version 2>/dev/null | head -n 1)"
  else report codex FAILED "codex did not read the agentdiet server"; failed=1; fi
else report codex skipped "not installed"; fi
if command -v claude > /dev/null 2>&1; then
  if HOME="$tmp/home" DISABLE_AUTOUPDATER=1 claude mcp list 2> /dev/null | grep -q 'agentdiet:.*Connected'; then
    report claude connected "$(claude --version 2>/dev/null | head -n 1)"
  else report claude FAILED "claude could not connect to the agentdiet server"; failed=1; fi
else report claude skipped "not installed"; fi
if command -v gemini > /dev/null 2>&1; then
  if HOME="$tmp/home" gemini mcp list 2> /dev/null | grep -qi 'agentdiet.*connected'; then
    report gemini connected "$(gemini --version 2>/dev/null | head -n 1)"
  else report gemini FAILED "gemini could not connect to the agentdiet server"; failed=1; fi
else report gemini skipped "not installed"; fi
exit $failed
