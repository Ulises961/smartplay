#!/usr/bin/env bash
# The push half of the main-branch guard: refuses to update or delete main/master on the remote.
# git passes the refs being pushed on stdin as "<local ref> <local sha> <remote ref> <remote sha>".
#
# What it does not stop: `git push --no-verify`, or a clone without `lefthook install`. Pushing a branch
# or a tag is allowed.
set -euo pipefail

while read -r local_ref local_sha remote_ref remote_sha; do
  case "$remote_ref" in
    refs/heads/main|refs/heads/master)
      cat >&2 <<MSG
BLOCKED: this push would update or delete '${remote_ref#refs/heads/}' on the remote.

Push a branch and open a pull request instead:
  git push -u origin <your-branch>
MSG
      exit 1
      ;;
  esac
done
exit 0
