#!/usr/bin/env bash
# Regenerates the *.in fixtures from a scratch jj repo, then run `BLESS=1 cargo test` and review the *.out diff.
# Identity, timestamps and change ids are fixed so the output only changes when jj's output format changes.
set -euo pipefail

fixtures=$(cd "$(dirname "$0")" && pwd)
repo=$(mktemp -d)
trap 'rm -rf "$repo"' EXIT
cd "$repo"

export JJ_USER="Test User" JJ_EMAIL="test@example.com"
export JJ_TIMESTAMP="2026-01-01T00:00:00+00:00" JJ_OP_TIMESTAMP="2026-01-01T00:00:00+00:00"
export JJ_OP_HOSTNAME="host" JJ_OP_USERNAME="user"
export JJ_CONFIG=/dev/null TZ=UTC

# every call needs another seed, otherwise all changes get the same change id
seed=0
jj() {
    seed=$((seed + 1))
    JJ_RANDOMNESS_SEED=$seed command jj --no-pager --config snapshot.auto-track='none()' "$@"
}
track() { jj --quiet file track "$@"; }
id() { jj log --no-graph -r "$1" -T 'change_id.short()'; }
dump() {
    local name=$1
    shift
    jj --color=always "$@" > "$fixtures/$name.in" 2>&1
}

jj --quiet git init .

# first: files of all kinds
mkdir -p src "dir with space"
echo a > src/a.rs
echo b > "dir with space/f(1).txt"
echo x > exe.sh && chmod +x exe.sh
ln -s src/a.rs link
printf '\xff\xfe latin1 \xe4\n' > bin.txt
echo keep > keep.txt
seq 1 20 > cf.txt
track .
jj --quiet describe -m "first commit with a really long summary line that definitely exceeds seventy-two chars"
first=$(id @)

# second: rename, copy, exec bit, removed and added symlink, non-UTF-8 change
jj --quiet new -m second
mkdir -p lib && mv src/a.rs lib/a.rs && echo c >> lib/a.rs
chmod -x exe.sh
rm link && ln -s keep.txt link2
echo b2 >> "dir with space/f(1).txt"
printf 'y\xff\n' >> bin.txt
cp keep.txt keep2.txt && echo m >> keep.txt
echo new > added.txt
track .
second=$(id @)

# typechange: file <-> symlink, exec bit set
jj --quiet new -m typechange
rm exe.sh && ln -s keep.txt exe.sh
rm link2 && echo f > link2
chmod +x added.txt
typechange=$(id @)

# moves inside directories
jj --quiet new -m moves
mkdir -p a/b && echo same > a/b/x.txt
track a
jj --quiet new -m mv
mkdir -p a/c && mv a/b/x.txt a/c/x.txt
mv keep2.txt keep3.txt
track .
mv_change=$(id @)

# conflict: two sides change the same line, merged
jj --quiet new "$first" -m side1
sed -i 's/^5$/five-a/' cf.txt
side1=$(id @)
jj --quiet new "$first" -m side2
sed -i 's/^5$/five-b/' cf.txt
side2=$(id @)
jj --quiet new "$side1" "$side2" -m merge
merge=$(id @)
jj --quiet new -m modconflict
sed -i 's/^1$/one/' cf.txt
modconflict=$(id @)
jj --quiet new -m resolve
seq 1 20 | sed 's/^5$/five/' > cf.txt
resolve=$(id @)
# rebasing a third change of the same line onto side1 creates the conflict in that change itself
jj --quiet new "$first" -m created
sed -i 's/^5$/five-c/' cf.txt
created=$(id @)
jj --quiet rebase -r "$created" -o "$side1"

# working copy on the conflicted merge with an untracked file
jj --quiet new "$modconflict"
echo untracked > untracked.txt
echo tracked > tracked.txt && track tracked.txt

short_log='template-aliases.short_log=
if(empty, label("empty", "-"), self.diff().files().len()) ++ " " ++
pad_end(4, change_id.shortest()) ++ " " ++
pad_end(50, if(self.description().len() == 0, label("empty", "-"), self.description().first_line())) ++ " " ++
label("bookmark", bookmarks.join(", "))'

dump status status
dump diff diff -r "$second"
dump diff_summary diff -r "$second" --summary
dump diff_stat diff -r "$second" --stat
dump diff_git diff -r "$second" --git
dump diff_types diff -r "$second" --types
dump diff_typechange diff -r "$typechange"
dump diff_moves diff -r "$mv_change"
dump diff_moves_summary diff -r "$mv_change" --summary
dump diff_conflict_created diff -r "$created"
dump diff_conflict_modified diff -r "$modconflict"
dump diff_conflict_resolved diff -r "$resolve"
dump resolve_list resolve --list -r "$merge"
dump log_short --config "$short_log" log -r 'all()' -T short_log --reversed
dump log_default log -r 'all()'
dump log_detailed log -r 'all()' -T builtin_log_detailed
dump log_patch log -r "$second|$modconflict" -p --summary
dump log_patch_git log -r "$second" -p --git
dump log_ascii --config ui.graph.style=ascii log -r 'all()'
dump show show -r "$second"
dump interdiff interdiff --from "$mv_change" --to "$mv_change-"
dump evolog evolog -r "$second"
