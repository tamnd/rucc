#!/usr/bin/env bash
# Packages one built binary into the two archives a release publishes, twice, and fails if the two
# runs disagree.
#
# `spec/cross-compile/13-distribution.md` section 13.6 asks for release artifacts that are
# reproducible: pinned inputs, `SOURCE_DATE_EPOCH`, no timestamps in archives, deterministic
# ordering everywhere. `tar -czf` and `zip -r` on a directory are none of those. They store the
# mtime of every file, which is when the checkout happened, they store the uid and the user name of
# whoever the runner runs as, gzip puts a timestamp in its own header, and both archivers take the
# files in whatever order the directory hands them over.
#
# So the timestamps are set to one value, the owner is zeroed, the file list is sorted before either
# archiver sees it, and the whole thing is done a second time into another directory and compared.
# The comparison is the part worth having. A reproducibility claim nobody checks is a claim that
# broke a year ago and nobody noticed, and section 13.6 says the check costs a CI job.
#
# usage: .github/package.sh <name> <binary> [outdir]
#
# <name> is the directory inside the archives and the stem of their names, which is
# rucc-<tag>-<target>. <binary> is the file to put in it. [outdir] defaults to dist.
#
# What this does not claim is that two different hosts produce the same archive. That would need the
# same tar and the same gzip on all five runners, and it is not what section 13.6 is about: the
# claim is that a release can be rebuilt and checked, which is per host and per target. The two
# flavours of tar on our runners want different flags for the same thing, GNU taking --owner and
# --mtime and bsdtar taking --uid and --gid and neither accepting the other's, so this asks which
# one it is talking to rather than guessing.

set -euo pipefail

name=${1:?the archive name, which is rucc-<tag>-<target>}
binary=${2:?the binary to package}
out=${3:-dist}

[ -f "$binary" ] || { echo "package: $binary is not there" >&2; exit 1; }

# The commit the tag points at, which is the one timestamp in a release that is a fact about the
# release rather than about the machine that built it. `SOURCE_DATE_EPOCH` wins if it is set,
# because that is what the variable is for and a caller who set it meant it.
epoch=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct)}

# `touch -t` reads its argument in local time, so the timezone is pinned as well as the stamp, or a
# runner in a different zone would write a different mtime from the same epoch. GNU date spells the
# conversion one way and BSD date spells it the other.
if ! stamp=$(date -u -d "@$epoch" +%Y%m%d%H%M.%S 2> /dev/null); then
  stamp=$(date -u -r "$epoch" +%Y%m%d%H%M.%S)
fi

sha256() {
  if command -v sha256sum > /dev/null; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# Everything that goes in, which is the binary and the three files somebody needs to know what they
# have: what it is, what they may do with it, and what changed.
stage() {
  local into=$1
  mkdir -p "$into/$name"
  cp "$binary" "$into/$name/"
  cp README.md LICENSE-APACHE CHANGELOG.md "$into/$name/"
  # Sorted here rather than by either archiver, because one of the two cannot sort. The names are
  # relative to the directory the archivers run in, which is how they end up in the archive.
  (cd "$into" && find "$name" -type f | LC_ALL=C sort > .files)
  # In the archive, not on disk: an mtime is a fact about a checkout and there is nothing in a
  # release it is true of.
  (cd "$into" && TZ=UTC0 xargs -I '{}' touch -t "$stamp" '{}' < .files)
}

# The tarball. `--format=ustar` because it is the format with no extended headers to carry a
# timestamp we did not ask for, `-T` because the list is already sorted, and `gzip -n` because the
# gzip header holds a timestamp and the name of the file it came from.
tarball() {
  local into=$1
  local flags=(--format=ustar --numeric-owner)
  if tar --version 2>&1 | grep -q 'GNU tar'; then
    flags+=(--owner=0 --group=0 "--mtime=@$epoch")
  else
    flags+=(--uid 0 --gid 0 --uname '' --gname '')
  fi
  (cd "$into" && tar "${flags[@]}" -cf - -T .files | gzip -9n > "$name.tar.gz")
}

# The zip, because the people who want one and the people who want a tarball do not overlap and
# neither group should have to install a tool. `-X` drops the uid, the gid and the platform extras,
# which is the same zeroing the tar flags do.
zipfile() {
  local into=$1
  rm -f "$into/$name.zip"
  if command -v zip > /dev/null; then
    (cd "$into" && zip -X -9 -q "$name.zip" -@ < .files)
  else
    # Windows, where bash is there and zip is not. 7-Zip takes the same sorted list through a
    # response file and stores the mtimes, which are the ones set above.
    (cd "$into" && 7z a -tzip -mx=9 "$name.zip" "@.files" > /dev/null)
  fi
}

build() {
  local into=$1
  rm -rf "$into"
  stage "$into"
  tarball "$into"
  zipfile "$into"
  rm -f "$into/.files"
}

build "$out"
build "$out/.again"

# The check. Two runs of the same inputs through the same archivers, and what is published is only
# published if they agreed.
for archive in "$name.tar.gz" "$name.zip"; do
  first=$(sha256 "$out/$archive")
  second=$(sha256 "$out/.again/$archive")
  if [ "$first" != "$second" ]; then
    echo "package: $archive is not reproducible: $first then $second" >&2
    exit 1
  fi
  echo "$archive $first"
  # Beside the archive, in the shape sha256sum reads, so that whoever downloads one can check it
  # with the tool they already have and with the name they already have.
  printf '%s  %s\n' "$first" "$archive" > "$out/$archive.sha256"
done

rm -rf "$out/.again"
