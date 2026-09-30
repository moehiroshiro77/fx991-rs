# Releasing

A release is a tag.  Everything else -- the three archives, the notes, the
checksums, the GitHub release itself -- is built from it by
`.github/workflows/release.yml`.

```bash
scripts/tag-release.sh 0.1.0 --push
```

That is the whole procedure once the version in `Cargo.toml` has been bumped and
committed.  `scripts/tag-release.sh` refuses to tag a dirty working tree, refuses
a version that disagrees with the workspace manifest, and pushes an annotated tag;
the push starts the workflow.

Doing it by hand is the same thing without the checks:

```bash
git tag -a v0.1.0 -m "fx991cnx 0.1.0"
git push origin v0.1.0
```

## What the workflow does

`verify` runs the test suite and nothing is built if it fails.  `notes` generates
the release text from the commit history.  `build` runs on three platforms and
uploads one archive each.  `release` checksums those archives and publishes them.

The notes are the commit history, grouped by the Conventional Commits prefix on
each subject, plus a fixed block describing the downloads, the licence, and the
firmware the archives do not contain.  They are produced by
`scripts/release-notes.sh`, which is git and the shell only -- no notes generator,
no network.  Run it locally to see what a release will say:

```bash
scripts/release-notes.sh v0.1.0
scripts/release-notes.sh v0.2.0 v0.1.0     # explicit range
```

With one argument it finds the previous tag itself: the highest version tag that
is an ancestor of the one being released.  The first release has none and uses the
whole history.

## The tag must match Cargo.toml

The version is compiled into the binaries, so a tag that disagrees with
`[workspace.package] version` would ship a binary reporting the wrong number.  The
build fails on that mismatch rather than publishing it.  Bump the version, commit
it, and tag that commit:

```bash
# edit Cargo.toml: version = "0.2.0"
git commit -am "chore: release 0.2.0"
scripts/tag-release.sh 0.2.0 --push
```

A tag with a suffix such as `v0.2.0-rc.1` is published as a prerelease.

## Rebuilding a release

Re-run the workflow from the Actions tab (`workflow_dispatch`, giving it the tag),
or push the tag again:

```bash
git push --force origin v0.1.0
```

Either way the existing release is updated in place: `gh release upload --clobber`
replaces the assets rather than failing on them, so a release that half-succeeded
after a runner hiccup can be finished by running it again.

## Checksums

The checksum manifest is generated in the `release` job, once, from the bytes that
are about to be uploaded -- not per platform while the archives are still being
built.  It is named SHA256SUMS.txt and is attached to the release beside the
archives.  Generating it in one place keeps one definition of what a checksum
means for the release, and it happens on the one runner where `sha256sum` is
guaranteed to be present (macOS has only `shasum -a 256`, and the Windows runner's
default shell is PowerShell).

The file is in GNU format, so a downloader checks it directly:

```bash
sha256sum --check SHA256SUMS.txt          # Linux, WSL, Git Bash
shasum -a 256 --check SHA256SUMS.txt      # macOS
```

On Windows, `Get-FileHash` or `certutil -hashfile <archive> SHA256` prints the
same digest to compare by eye.

## Cargo.lock is not tracked

`.gitignore` lists `Cargo.lock`, so the release build resolves dependencies at
build time and does not use `--locked`.  Two consequences worth knowing:

* A release is not byte-for-byte reproducible from the tag alone.  The source is;
  the dependency versions are whatever the registry resolved that day.
* If a dependency publishes a breaking change under a compatible version, a
  rebuild of an old tag can fail or differ.  Re-running a release is therefore
  best done soon after the original, not months later.

Tracking the lock file and building with `--locked` would remove both, at the cost
of a file the project currently keeps out of the repository.

## What the archives contain

Two binaries, the licence, and the two READMEs.  Nothing else -- in particular,
not the firmware.  The ROM image and the face texture are Casio's copyrighted
material and are not distributed here; the archives are built and smoke-tested
without them, and a user supplies their own copy of `data/rom_verF.bin` and
`data/skin.rgba` as the README describes.

Because the archives carry no firmware, the smoke test is the command line
answering `emu --help` from the unpacked archive.  The graphical front end needs a
GPU, which a CI runner does not have; asking the CLI for its usage text is what
proves the archive is intact, the executable bit survived packaging, and the
binary starts on that platform.
