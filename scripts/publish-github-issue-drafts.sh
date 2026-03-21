#!/bin/zsh

set -euo pipefail

draft_dir="${1:-docs/github-issues}"

if ! command -v gh >/dev/null 2>&1; then
  echo "gh CLI not found" >&2
  exit 1
fi

if [[ ! -d "$draft_dir" ]]; then
  echo "draft directory not found: $draft_dir" >&2
  exit 1
fi

for file in "$draft_dir"/[0-9][0-9]-*.md; do
  title="$(sed -n '1s/^Title: //p' "$file")"
  if [[ -z "$title" ]]; then
    echo "missing Title header in $file" >&2
    exit 1
  fi

  body_file="$(mktemp)"
  trap 'rm -f "$body_file"' EXIT
  sed '1d' "$file" > "$body_file"

  echo "Creating issue from $file"
  gh issue create --title "$title" --body-file "$body_file"
  rm -f "$body_file"
  trap - EXIT
done
