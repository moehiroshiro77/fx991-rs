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

# Release notes for a tag, built from the commit history.
#
#   scripts/release-notes.sh v0.1.0
#   scripts/release-notes.sh v0.2.0 v0.1.0
#
# The notes are grouped by the Conventional Commits prefix on each subject, so
# the history is the only thing anybody has to maintain.  Commit bodies here are
# prose about *why* a change was made, and that is the right place for it: a
# release note that paraphrased them would be a second, staler copy.
#
# With one argument the previous tag is found automatically: the highest version
# tag that is an ancestor of the one being released.  The first release has none,
# and the whole history is used.
#
# This is git and the shell only.  No network, no third-party notes generator,
# for the same reason the workspace has no dependencies: what a release says has
# to be reproducible from the repository alone.
set -euo pipefail

if [ $# -lt 1 ] || [ $# -gt 2 ]; then
    echo "usage: scripts/release-notes.sh <tag> [previous-tag]" >&2
    exit 2
fi

cd "$(git rev-parse --show-toplevel)"

tag=$1
prev=${2:-}

if ! git rev-parse --verify --quiet "refs/tags/$tag" > /dev/null; then
    echo "release-notes: this repository has no tag named $tag" >&2
    exit 1
fi

if [ -z "$prev" ]; then
    while read -r candidate; do
        if [ "$candidate" = "$tag" ]; then
            continue
        fi
        if git merge-base --is-ancestor "$candidate" "$tag" 2> /dev/null; then
            prev=$candidate
            break
        fi
    done < <(git tag --list 'v[0-9]*' --sort=-v:refname)
fi

if [ -n "$prev" ]; then
    range="$prev..$tag"
else
    range="$tag"
fi

version=${tag#v}
tab=$'\t'

# One section of the notes, from the commits whose subject starts with one of the
# given types.  A scope, when there is one, is pulled forward as bold text.
emit() {
    heading=$1
    pattern=$2
    entries=$(git log --no-merges --format="%h%x09%s" "$range" \
        | grep -E "^[0-9a-f]+${tab}(${pattern})" || true)
    if [ -z "$entries" ]; then
        return 0
    fi
    printf '## %s\n\n' "$heading"
    while IFS="$tab" read -r sha subject; do
        subject=$(printf '%s' "$subject" \
            | sed -E 's/^[a-z]+\(([^)]*)\)!?: /**\1**: /; s/^[a-z]+!?: //')
        printf -- '- %s (%s)\n' "$subject" "$sha"
    done <<< "$entries"
    printf '\n'
}

printf '# fx991cnx %s\n\n' "$version"
cat <<'INTRO'
An emulator for the **Casio fx-991CN X** (VerF) that runs the calculator's real
firmware.  The `emu` binary is the scriptable command line; `fx991cnx` is the
clickable window.
INTRO
printf '\n'

# A change that breaks compatibility is the first thing a reader needs, whether
# or not the subject says so: anyone upgrading has to know before anything else.
#
# Conventional Commits declares a break rather than leaving it to be inferred -- a
# `!` before the colon in the subject, or a `BREAKING CHANGE:` trailer -- so that
# is what is looked for.  Searching for the word "breaking" in the prose would
# instead list every commit that happens to mention it.
breaking=$(
    {
        git log --no-merges --format="%h%x09%s" "$range" \
            | grep -E "^[0-9a-f]+${tab}[a-z]+(\([^)]*\))?!: " || true
        git log --no-merges --grep='^BREAKING[ -]CHANGE:' --format="%h%x09%s" "$range" || true
    } | sort -u
)
if [ -n "$breaking" ]; then
    printf '## Breaking changes\n\n'
    while IFS="$tab" read -r sha subject; do
        subject=$(printf '%s' "$subject" \
            | sed -E 's/^[a-z]+\(([^)]*)\)!?: /**\1**: /; s/^[a-z]+!?: //')
        printf -- '- %s (%s)\n' "$subject" "$sha"
    done <<< "$breaking"
    printf '\n'
fi

# The commit types that get a section of their own.  Kept in one place because
# the catch-all below has to know exactly which subjects the sections above have
# already claimed.
TYPES='feat|fix|perf|refactor|docs|chore|test|ci|build|style'

# The grammar Conventional Commits defines: a type, an optional scope, an
# optional `!`, a colon, and a description.
emit 'Features' 'feat(\([^)]*\))?!?: '
emit 'Fixes' 'fix(\([^)]*\))?!?: '
emit 'Other changes' '(perf|refactor|docs|chore|test|ci|build|style)(\([^)]*\))?!?: '

# Everything the sections above did not take: a subject with no type at all, one
# that is nearly a commit type but not quite (`fix:no space`), and a type nobody
# has used before (`random: ...`).  The last is the reason this exists -- a
# release that silently drops a change is worse than one that lists it under a
# vague heading.
unclassified=$(git log --no-merges --format="%h%x09%s" "$range" \
    | grep -Ev "^[0-9a-f]+${tab}(${TYPES})(\([^)]*\))?!?: " || true)
if [ -n "$unclassified" ]; then
    printf '## Unclassified\n\n'
    while IFS="$tab" read -r sha subject; do
        printf -- '- %s (%s)\n' "$subject" "$sha"
    done <<< "$unclassified"
    printf '\n'
fi

printf '## Downloads\n\n'
printf '| archive | platform |\n|---|---|\n'
printf '| fx991cnx-%s-windows-x86_64.zip | Windows 10 or later, x86_64: emu.exe and fx991cnx.exe |\n' "$version"
printf '| fx991cnx-%s-linux-x86_64.tar.gz | Linux, x86_64; the window needs the usual X11 or Wayland libraries |\n' "$version"
printf '| fx991cnx-%s-macos-universal.tar.gz | macOS 11 or later, Intel and Apple silicon in one binary |\n' "$version"
printf '\n'
printf 'Extract an archive and run the binaries from there.  Each one holds the two\n'
printf 'binaries and the licence; the hashes are listed in the SHA256SUMS.txt\n'
printf 'attached beside them.\n\n'

cat <<'FIRMWARE'
## The firmware is not in these archives

The ROM image and the face texture are Casio's copyrighted material, so they are
not part of this project and are not distributed with it.  The emulator needs
both before it can start, and the repository's README says what to supply:

| file | what | |
|---|---|---|
| data/rom_verF.bin | the ROM image | 262 144 bytes, MD5 47bbf88fb3a9432b311b423f9b766e8f |
| data/skin.rgba | the face texture | 307x615, 8-bit RGBA, no header -- 755 220 bytes |

Put them in a data directory beside the binaries, or point at them with --rom and
--skin.  Nothing in these archives will run without them.

This project is not affiliated with or endorsed by Casio.
FIRMWARE
printf '\n'

cat <<'LICENCE'
## Licence

**GPL-3.0-only**, with no per-crate exception.  The source this release was built
from is in the repository at this tag, and distributing a modified version means
publishing its source under the same licence.  The firmware above is not covered
by it.
LICENCE
printf '\n'

# Owner and repository, for links that outlive the release.  Actions provides the
# slug; a local run falls back to the remote.
repo=${GITHUB_REPOSITORY:-}
if [ -z "$repo" ]; then
    origin=$(git config --get remote.origin.url 2> /dev/null || true)
    repo=$(printf '%s' "$origin" \
        | sed -E 's#^git@[^:]+:##; s#^https?://[^/]+/##; s#\.git$##')
fi

if [ -n "$repo" ]; then
    if [ -n "$prev" ]; then
        printf '**Full Changelog**: https://github.com/%s/compare/%s...%s\n' "$repo" "$prev" "$tag"
    else
        printf '**Full Changelog**: https://github.com/%s/commits/%s\n\n' "$repo" "$tag"
        printf 'First release: every commit in it is listed above.\n'
    fi
fi
