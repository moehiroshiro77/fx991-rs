#!/usr/bin/env bash
#
# SPDX-License-Identifier: GPL-3.0-only
#
# Copyright (C) 2026 fx991-rs contributors
#
# This program is free software: you can redistribute it and/or modify it under
# the terms of the GNU General Public License as published by the Free Software
# Foundation, version 3.  It is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY
# or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU General Public License in
# LICENSE for more details.

# Cut a release tag, or push one that already exists.
#
#   scripts/tag-release.sh 0.1.0            # tag locally
#   scripts/tag-release.sh 0.1.0 --push     # tag and push, which starts the release
#
# The version must be the one Cargo.toml declares, because the release workflow
# refuses a tag that disagrees with it.  Being told here beats pushing a tag,
# waiting for the build, and reading the failure afterwards.  docs/releasing.md
# has the whole procedure.
set -euo pipefail

if [ $# -lt 1 ]; then
    echo "usage: scripts/tag-release.sh <version> [--push]" >&2
    exit 2
fi

version=${1#v}
push=0
case "${2:-}" in
    --push) push=1 ;;
    '') ;;
    *)
        echo "usage: scripts/tag-release.sh <version> [--push]" >&2
        exit 2
        ;;
esac

cd "$(git rev-parse --show-toplevel)"

# A tag outlives the working tree, so a half-finished edit must not be able to
# become one.
if [ -n "$(git status --porcelain)" ]; then
    echo "tag-release: the working tree still has changes; commit or stash them first" >&2
    git status --short >&2
    exit 1
fi

declared=$(awk '
    /^\[workspace\.package\]/ { inside = 1 }
    inside && /^version *=/ { gsub(/[^0-9.]/, ""); print; exit }
' Cargo.toml)
if [ "$declared" != "$version" ]; then
    echo "tag-release: Cargo.toml declares $declared, not $version" >&2
    echo "Bump [workspace.package] version, commit it, and run this again." >&2
    exit 1
fi

if git rev-parse --verify --quiet "refs/tags/v$version" > /dev/null; then
    echo "tag-release: v$version is already tagged" >&2
    exit 1
fi

# Annotated: a release tag is a statement about a commit, and it is dated and
# attributed like one.
git tag -a "v$version" -m "fx991cnx $version"
echo "tagged v$version"

if [ "$push" = 1 ]; then
    git push origin "v$version"
    echo "pushed v$version: the release workflow is building the archives"
else
    echo "push it when ready: git push origin v$version"
fi
