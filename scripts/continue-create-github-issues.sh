#!/bin/zsh

set -euo pipefail

REPO="${GH_REPO:-l1x/spinr}"
DRAFT_DIR="${1:-docs/github-issues}"

if ! command -v gh >/dev/null 2>&1; then
  echo "gh CLI not found" >&2
  exit 1
fi

if [[ ! -d "$DRAFT_DIR" ]]; then
  echo "draft directory not found: $DRAFT_DIR" >&2
  exit 1
fi

echo "Repo: $REPO"
echo "Draft dir: $DRAFT_DIR"

for file in "$DRAFT_DIR"/[0-9][0-9]-*.md; do
  title="$(sed -n '1s/^Title: //p' "$file")"
  if [[ -z "$title" ]]; then
    echo "missing Title header in $file" >&2
    exit 1
  fi

  existing_url="$(
    gh issue list \
      --repo "$REPO" \
      --state all \
      --limit 200 \
      --json title,url \
      --jq '.[] | [.title, .url] | @tsv' |
      awk -F $'\t' -v title="$title" '$1 == title { print $2; exit }'
  )"

  if [[ -n "$existing_url" ]]; then
    echo "Skipping existing issue: $title"
    echo "  $existing_url"
    continue
  fi

  body_file="$(mktemp)"
  trap 'rm -f "$body_file"' EXIT
  sed '1d' "$file" > "$body_file"

  echo "Creating issue from $file"
  gh issue create --repo "$REPO" --title "$title" --body-file "$body_file"

  rm -f "$body_file"
  trap - EXIT
done
