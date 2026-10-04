#!/bin/sh
# Run in a clean, non-shallow working repository:
#   bash /path/to/sync-upstream.sh UPSTREAM_URL PINNED_UPSTREAM_SHA OUTPUT_DIR
# OUTPUT_DIR must be outside the worktree. Success leaves the candidate at HEAD,
# writes candidate.bundle (self-contained HEAD) and candidate.sha, and prints
# base_sha/candidate_sha/changed as key=value lines. Conflicts abort the merge.
# This prepares a candidate only: it never pushes, tests or installs anything.
set -eu

fail() { printf 'upstream sync: %s\n' "$*" >&2; exit 1; }
[ "$#" -eq 3 ] || fail 'usage: sync-upstream.sh UPSTREAM_URL PINNED_UPSTREAM_SHA OUTPUT_DIR'
upstream_url=$1
upstream_sha=$2
output_dir=$3
case "$upstream_sha" in ''|*[!0-9a-f]*) fail 'upstream SHA must be 40 lowercase hexadecimal characters' ;; esac
[ "${#upstream_sha}" -eq 40 ] || fail 'upstream SHA must be 40 lowercase hexadecimal characters'
[ "$(git rev-parse --is-inside-work-tree)" = true ] || fail 'a working repository is required'
[ "$(git rev-parse --is-shallow-repository)" = false ] || fail 'a full, non-shallow history is required'
status=$(git status --porcelain=v1 --untracked-files=all)
[ -z "$status" ] || fail 'refusing to modify a dirty working tree (including untracked files)'
if git rev-parse -q --verify MERGE_HEAD >/dev/null; then
  fail 'finish or abort the existing merge before preparing a candidate'
fi
base_sha=$(git rev-parse --verify 'HEAD^{commit}')
mkdir -p "$output_dir"
output_dir=$(CDPATH= cd "$output_dir" && pwd -P)
worktree=$(git rev-parse --show-toplevel)
worktree=$(CDPATH= cd "$worktree" && pwd -P)
case "$output_dir/" in "$worktree/"*) fail 'output directory must be outside the worktree' ;; esac
[ ! -e "$output_dir/candidate.bundle" ] && [ ! -e "$output_dir/candidate.sha" ] || fail 'candidate output files already exist'

git -c core.hooksPath=/dev/null fetch --no-tags -- "$upstream_url" "$upstream_sha"
[ "$(git rev-parse --verify 'FETCH_HEAD^{commit}')" = "$upstream_sha" ] || fail 'fetched commit did not match the pinned upstream SHA'
if git merge-base --is-ancestor "$upstream_sha" "$base_sha"; then
  changed=false
else
  # Fixed metadata makes identical parent commits produce the same candidate.
  timestamp=$(git show -s --format=%ct "$base_sha")
  upstream_time=$(git show -s --format=%ct "$upstream_sha")
  [ "$upstream_time" -le "$timestamp" ] || timestamp=$upstream_time
  if ! GIT_AUTHOR_NAME='github-actions[bot]' \
       GIT_AUTHOR_EMAIL='41898282+github-actions[bot]@users.noreply.github.com' \
       GIT_COMMITTER_NAME='github-actions[bot]' \
       GIT_COMMITTER_EMAIL='41898282+github-actions[bot]@users.noreply.github.com' \
       GIT_AUTHOR_DATE="$timestamp +0000" GIT_COMMITTER_DATE="$timestamp +0000" \
       git -c core.hooksPath=/dev/null -c commit.gpgsign=false merge --no-ff \
         -m "Merge upstream $upstream_sha" "$upstream_sha" >&2; then
    conflicts=$(git diff --name-only --diff-filter=U)
    printf 'upstream sync: merge failed; no candidate will be published. Conflicting files:\n%s\n' "$conflicts" >&2
    if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
      {
        printf '\n## Upstream merge failed\n\nNo publish. Conflicting files (empty means a non-conflict merge error):\n\n```text\n'
        printf '%s\n' "$conflicts"
        printf '```\n'
      } >> "$GITHUB_STEP_SUMMARY"
    fi
    if git rev-parse -q --verify MERGE_HEAD >/dev/null; then
      git -c core.hooksPath=/dev/null merge --abort
    fi
    fail 'merge aborted; resolve upstream integration manually in a separate checkout'
  fi
  changed=true
fi
candidate_sha=$(git rev-parse --verify 'HEAD^{commit}')
git merge-base --is-ancestor "$base_sha" "$candidate_sha"
git merge-base --is-ancestor "$upstream_sha" "$candidate_sha"
git bundle create "$output_dir/candidate.bundle" HEAD
printf '%s\n' "$candidate_sha" > "$output_dir/candidate.sha"
printf 'base_sha=%s\ncandidate_sha=%s\nchanged=%s\n' "$base_sha" "$candidate_sha" "$changed"
