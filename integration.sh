#!/usr/bin/env bash
# Builds slint/integration-branch of barafael/slint: upstream master, each branch below merged
# into it in order, and this script on top. Run it in any clone of Slint:
#
#   ./integration.sh        build the branch and print what it is
#   ./integration.sh push   build it, tag the old tip integration/<date>-<commit>, push both
#
# The script never resolves a conflict: when a branch stops merging, rebase that branch onto
# master (its pull request needs that anyway) and run it again. Edit the list here, on the
# branch; once a pull request has landed, delete its line.
set -euo pipefail

UPSTREAM=https://github.com/slint-ui/slint.git
FORK=git@github.com:barafael/slint.git
BRANCH=slint/integration-branch

TOPICS=(
    no-op-animation            # slint-ui/slint#14823
    scrollbar-thumb-animation  # slint-ui/slint#14841
    text-cursor-blink-timeout  # slint-ui/slint#14855
    inline-code-padding        # slint-ui/slint#14721
    edualc-text                # edualc's, no pull request: loose lists, line-height-factor, language
    incremental-text-shaping   # edualc's, no pull request yet: only changed paragraphs are shaped again
)

script=$(git hash-object -w "$0")

git fetch -q --no-tags "$UPSTREAM" +refs/heads/master:refs/integration/master
git fetch -q --no-tags "$FORK" "${TOPICS[@]/*/+refs/heads/&:refs/integration/&}"
old=$(git ls-remote "$FORK" "refs/heads/$BRANCH" | cut -f1)
[ -z "$old" ] || git fetch -q --no-tags "$FORK" "+refs/heads/$BRANCH:refs/integration/old"

tip=$(git rev-parse refs/integration/master)
for topic in "${TOPICS[@]}"; do
    if ! tree=$(git merge-tree --write-tree --name-only "$tip" "refs/integration/$topic"); then
        echo "$topic does not merge (files below): rebase it onto master and run this again." >&2
        echo "$tree" | sed -n '2,/^$/p' >&2
        exit 1
    fi
    tip=$(git commit-tree "$tree" -p "$tip" -p "refs/integration/$topic" -m "Merge branch '$topic'")
done

index=$(mktemp -d)/index
GIT_INDEX_FILE=$index git read-tree "$tip"
GIT_INDEX_FILE=$index git update-index --add --cacheinfo "100755,$script,integration.sh"
tree=$(GIT_INDEX_FILE=$index git write-tree)
rm -r "$(dirname "$index")"
tip=$(git commit-tree "$tree" -p "$tip" -m "integration.sh: the script that builds this branch")

if [ -n "$old" ] && [ "$(git rev-parse "$old^{tree}")" = "$tree" ]; then
    echo "$BRANCH is up to date: $old"
    exit 0
fi
git log --oneline --first-parent "refs/integration/master~1..$tip"

if [ "${1:-}" = push ]; then
    refs=("+$tip:refs/heads/$BRANCH")
    [ -z "$old" ] || refs+=("$old:refs/tags/integration/$(date +%F)-$(git rev-parse --short "$old")")
    git push --atomic --force-with-lease="refs/heads/$BRANCH:$old" "$FORK" "${refs[@]}"
    echo "Pushed $BRANCH: $tip"
else
    echo "Built $tip; run with 'push' to publish it."
fi
