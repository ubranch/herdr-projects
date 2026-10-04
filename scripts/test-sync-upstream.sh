#!/usr/bin/env bash
# Run: bash scripts/test-sync-upstream.sh (Git Bash on Windows or Bash on Ubuntu).
set -euo pipefail
sync_script=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)/sync-upstream.sh
tmp=$(mktemp -d /tmp/test-sync-upstream.XXXXXX)
trap 'rm -rf -- "$tmp"' EXIT
trap 'exit 1' HUP INT TERM
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_TERMINAL_PROMPT=0
upstream=$tmp/upstream
fork=$tmp/fork
mkdir "$tmp/hooks"

fail() {
  printf 'sync-upstream test: %s\n' "$*" >&2
  if [ -f "$tmp/sync.log" ]; then cat "$tmp/sync.log" >&2; fi
  exit 1
}
configure() {
  git -C "$1" config --local user.name 'Sync regression'
  git -C "$1" config --local user.email 'sync-test@example.invalid'
  git -C "$1" config --local commit.gpgsign false
  git -C "$1" config --local core.hooksPath "$tmp/hooks"
  git -C "$1" config --local core.autocrlf false
}
commit() {
  git -C "$1" add --all
  git -C "$1" commit -qm "$2"
}
run_sync() {
  (cd "$fork" && GITHUB_STEP_SUMMARY= bash "$sync_script" "$upstream" "$upstream_sha" "$1") > "$tmp/sync.log" 2>&1
}

git init -q -b main --template="$tmp/hooks" "$upstream"
configure "$upstream"
printf 'upstream base\n' > "$upstream/upstream.txt"
printf 'shared base\n' > "$upstream/shared.txt"
commit "$upstream" 'Common base'
git -c core.autocrlf=false -c core.hooksPath="$tmp/hooks" clone -q --template="$tmp/hooks" "$upstream" "$fork"
configure "$fork"
printf 'Windows-only customization\n' > "$tmp/windows.expected"
cp "$tmp/windows.expected" "$fork/windows-only.txt"
commit "$fork" 'Windows customization'
windows_sha=$(git -C "$fork" rev-parse HEAD)
printf 'upstream update\nsecond line\n' > "$tmp/upstream.expected"
cp "$tmp/upstream.expected" "$upstream/upstream.txt"
commit "$upstream" 'Divergent upstream update'
upstream_sha=$(git -C "$upstream" rev-parse HEAD)

# A real divergent merge retains Windows bytes and incorporates upstream bytes.
run_sync "$tmp/merged" || fail 'divergent merge failed'
candidate_sha=$(git -C "$fork" rev-parse HEAD)
cmp -s "$tmp/windows.expected" "$fork/windows-only.txt" || fail 'Windows customization changed'
cmp -s "$tmp/upstream.expected" "$fork/upstream.txt" || fail 'upstream update missing'
git -C "$fork" merge-base --is-ancestor "$windows_sha" "$candidate_sha" || fail 'Windows history lost'
git -C "$fork" merge-base --is-ancestor "$upstream_sha" "$candidate_sha" || fail 'upstream history lost'
[ "$(cat "$tmp/merged/candidate.sha")" = "$candidate_sha" ] || fail 'candidate SHA differs from source HEAD'
git -c core.autocrlf=false -c core.hooksPath="$tmp/hooks" clone -q --template="$tmp/hooks" "$tmp/merged/candidate.bundle" "$tmp/bundle-clone"
[ "$(git -C "$tmp/bundle-clone" rev-parse HEAD)" = "$candidate_sha" ] || fail 'bundle clone has a different candidate'
cmp -s "$tmp/windows.expected" "$tmp/bundle-clone/windows-only.txt" || fail 'bundle lost Windows bytes'
cmp -s "$tmp/upstream.expected" "$tmp/bundle-clone/upstream.txt" || fail 'bundle lost upstream bytes'

# Repeating the same pinned upstream commit must not create another source commit.
run_sync "$tmp/already-merged" || fail 'already-merged sync failed'
[ "$(git -C "$fork" rev-parse HEAD)" = "$candidate_sha" ] || fail 'already-merged sync changed HEAD'

# Untracked user data is dirty too; rejection must leave data and HEAD untouched.
printf 'untracked user data\nsecond line\n' > "$tmp/user.expected"
cp "$tmp/user.expected" "$fork/user-data.txt"
if run_sync "$tmp/dirty"; then fail 'accepted untracked user data'; fi
cmp -s "$tmp/user.expected" "$fork/user-data.txt" || fail 'dirty rejection changed user data'
[ "$(git -C "$fork" rev-parse HEAD)" = "$candidate_sha" ] || fail 'dirty rejection changed HEAD'
rm -- "$fork/user-data.txt"

# Opposing edits to the same base line force an actual merge conflict.
printf 'Windows shared update\n' > "$tmp/shared.expected"
cp "$tmp/shared.expected" "$fork/shared.txt"
commit "$fork" 'Windows shared edit'
windows_sha=$(git -C "$fork" rev-parse HEAD)
printf 'upstream shared update\n' > "$upstream/shared.txt"
commit "$upstream" 'Conflicting upstream edit'
upstream_sha=$(git -C "$upstream" rev-parse HEAD)
[ -z "$(git -C "$fork" status --porcelain=v1 --untracked-files=all)" ] || fail 'conflict fixture is dirty'
if run_sync "$tmp/conflict"; then fail 'accepted conflicting edits'; fi
[ "$(git -C "$fork" rev-parse ORIG_HEAD)" = "$windows_sha" ] || fail 'conflicting merge was not attempted'
[ "$(git -C "$fork" rev-parse HEAD)" = "$windows_sha" ] || fail 'conflict changed Windows HEAD'
[ -z "$(git -C "$fork" status --porcelain=v1 --untracked-files=all)" ] || fail 'conflict left a dirty worktree'
if git -C "$fork" rev-parse -q --verify MERGE_HEAD >/dev/null; then fail 'conflict left a merge in progress'; fi
cmp -s "$tmp/windows.expected" "$fork/windows-only.txt" || fail 'conflict changed Windows bytes'
cmp -s "$tmp/shared.expected" "$fork/shared.txt" || fail 'conflict changed original shared bytes'
cmp -s "$tmp/upstream.expected" "$fork/upstream.txt" || fail 'conflict changed previously merged bytes'
[ "$(git -C "$upstream" rev-parse HEAD)" = "$upstream_sha" ] || fail 'conflict changed upstream HEAD'
[ ! -e "$tmp/conflict/candidate.bundle" ] && [ ! -e "$tmp/conflict/candidate.sha" ] || fail 'conflict published candidate artifacts'
printf 'sync-upstream: behavioral checks passed\n'
