#!/usr/bin/env bash
# Publishes the workspace to crates.io, and can be run a second time to finish what a first
# run started.
#
# `cargo publish --workspace` works out the dependency order itself and is what does the
# work here. The two things it does not do are why this script exists. crates.io rate limits
# updates to existing crates, so a workspace this size gets a 429 partway through often
# enough to plan for, and a second call after one of those stops at the first crate that is
# already up rather than carrying on past it. So this asks the index what is already there,
# excludes it, and waits when the registry tells it to wait.
#
# Run from the root of the workspace with CARGO_REGISTRY_TOKEN set. Running it when
# everything is already published is a no-op that exits zero, which is what makes re-running
# the release job the way to recover from a partial upload.

set -euo pipefail

# The limit that bites is one new version of an existing crate per minute, with a burst that
# a workspace this size can drain in one release and that three releases in an afternoon will
# drain outright. So the worst case is a minute per crate left, and the number of attempts is
# the number of crates plus enough slack to be sure rather than a round number.
attempts=40
pause=70

metadata=$(cargo metadata --format-version 1 --no-deps)
version=$(echo "$metadata" | jq -r '.packages[] | select(.name == "rucc") | .version')
# `publish = []` is how a crate says it is not for the registry, which is what `xtask` says.
crates=$(echo "$metadata" | jq -r '.packages[] | select(.publish != []) | .name' | sort)
total=$(echo "$crates" | wc -w | tr -d ' ')

# Where a crate lives in the sparse index, which is by the length of its name and is the
# same rule every registry client implements.
index_path() {
  local name=$1
  case ${#name} in
    1) echo "1/$name" ;;
    2) echo "2/$name" ;;
    3) echo "3/${name:0:1}/$name" ;;
    *) echo "${name:0:2}/${name:2:2}/$name" ;;
  esac
}

# The index is asked rather than the API, because the index is what cargo itself reads and a
# crate that has never been published is a 404 there rather than an empty answer.
already_up() {
  curl --silent --fail "https://index.crates.io/$(index_path "$1")" 2>/dev/null |
    grep -q "\"vers\":\"$2\""
}

for attempt in $(seq 1 "$attempts"); do
  exclude=()
  up=0
  for crate in $crates; do
    if already_up "$crate" "$version"; then
      exclude+=(--exclude "$crate")
      up=$((up + 1))
    fi
  done

  if [ "$up" -eq "$total" ]; then
    echo "all $total crates are on crates.io at $version"
    exit 0
  fi

  # Cargo verifies every crate it is about to publish before it uploads any of them, so the
  # first attempt has already built each of these from its own tarball and a later attempt is
  # looking at the same tree. Verifying again is several minutes per attempt spent proving
  # something that was proven before the wait started, and it is what made ten attempts take
  # three quarters of an hour and still run out.
  verify=()
  if [ "$attempt" -gt 1 ]; then
    verify+=(--no-verify)
  fi

  echo "attempt $attempt: $up of $total already up, publishing the other $((total - up))"
  if cargo publish --workspace --locked "${exclude[@]}" "${verify[@]}" 2>&1 | tee /tmp/publish.log; then
    echo "published $((total - up)) crates at $version"
    exit 0
  fi

  if ! grep -q "429 Too Many Requests" /tmp/publish.log; then
    echo "the publish failed for a reason that waiting will not fix" >&2
    exit 1
  fi
  echo "crates.io asked for a slower pace, waiting ${pause}s and carrying on where it stopped"
  sleep "$pause"
done

echo "gave up after $attempts attempts, $up of $total crates are up at $version" >&2
exit 1
