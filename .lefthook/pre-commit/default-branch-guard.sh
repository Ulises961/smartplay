#!/usr/bin/env bash
# Refuses a commit made while main or master is checked out: changes reach main through a pull request.
#
# What it does not stop: `git commit --no-verify`, a clone that never ran `lefthook install`, and anything
# done on GitHub itself (merging a pull request is how main is meant to move). Run `git pull --ff-only` on
# main, since a merge commit made there would be refused too.
set -euo pipefail

# An empty repository has to receive its first commit on the default branch.
git rev-parse --verify --quiet HEAD >/dev/null || exit 0

# Empty during a rebase or cherry-pick (detached HEAD): those commits are not on main.
branch=$(git symbolic-ref --quiet --short HEAD || true)

case "$branch" in
  main|master)
    cat >&2 <<MSG
BLOCKED: you are on '$branch'. Commits go on a branch and reach $branch through a pull request.

  git switch -c <type>/<short-description>      # for example feat/normalizer, fix/playlist-order
MSG
    exit 1
    ;;
esac
exit 0
