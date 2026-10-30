#!/usr/bin/env bash
# Usage: sticky-comment.sh KEY BODY_FILE
# Posts BODY_FILE on pull request $PR, or replaces the bot comment an earlier run posted with the
# same KEY. Each workflow uses its own KEY, so their comments never overwrite each other (gh's
# --edit-last would edit whichever bot comment came last). Needs GH_TOKEN, GITHUB_REPOSITORY and PR.
set -euo pipefail

key=${1:?usage: sticky-comment.sh KEY BODY_FILE}
file=${2:?usage: sticky-comment.sh KEY BODY_FILE}
marker="<!-- dfdu-comment: $key -->"
body="$marker
$(cat "$file")"

id=$(gh api "repos/$GITHUB_REPOSITORY/issues/$PR/comments" --paginate \
  --jq ".[] | select(.user.type == \"Bot\" and (.body | startswith(\"$marker\"))) | .id" | head -n 1)

if [ -n "$id" ]; then
  gh api -X PATCH "repos/$GITHUB_REPOSITORY/issues/comments/$id" -f body="$body" >/dev/null
else
  gh api -X POST "repos/$GITHUB_REPOSITORY/issues/$PR/comments" -f body="$body" >/dev/null
fi
echo "commented $key on #$PR"
