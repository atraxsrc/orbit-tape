#!/usr/bin/env sh
# Fails when local-only files are tracked or staged.
#
#   scripts/check-private-files.sh           check every tracked file (CI)
#   scripts/check-private-files.sh --staged  check the files staged for commit
#
# Local notes, assistant config, private Pine sources, the personal scan list
# and shell dotfiles from a home directory never belong in this repository.

set -eu

if [ "${1:-}" = "--staged" ]; then
    files=$(git diff --cached --name-only --diff-filter=ACMR)
else
    files=$(git ls-files)
fi

blocked=$(printf '%s\n' "$files" | grep -E \
    -e '(^|/)start-here\.md$' \
    -e '(^|/)CLAUDE(\.local)?\.md$' \
    -e '(^|/)AGENTS\.md$' \
    -e '(^|/)\.claude/' \
    -e '^pine-sources/' \
    -e '(^|/)scan-symbols\.txt$' \
    -e '(^|/)\.env(\..*)?$' \
    -e '^\.(bashrc|bash_profile|profile|zshrc|zprofile|gitconfig|ripgreprc|mcp\.json)$' \
    -e '^\.idea/' \
    || true)

if [ -n "$blocked" ]; then
    echo "These files are local-only and must not be committed:" >&2
    printf '  %s\n' $blocked >&2
    exit 1
fi
