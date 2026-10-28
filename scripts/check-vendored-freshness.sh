#!/usr/bin/env bash
# check-vendored-freshness.sh — verify the Enclave vendored copy of agent_core
# is fresh, agent_core side only (FRG-38).
#
# The vendored tree at <enclave>/third_party/agent_core carries a record file
# `.agent_core-source-sha` containing the full agent_core commit SHA the
# snapshot was taken from (written by the Enclave-side re-vendor; see
# docs/vendoring.md). This script compares that recorded SHA against an
# expected SHA (default: this repo's HEAD) and fails loudly on mismatch or on
# a missing record/tree, so a silently drifted vendored copy can never pass.
#
# Usage:
#   check-vendored-freshness.sh [--expected-sha <sha>] [<enclave-checkout>]
#   check-vendored-freshness.sh --self-test
#
# <enclave-checkout> defaults to $ENCLAVE_CHECKOUT or ../enclave relative to
# this repo root. --self-test simulates a vendored copy in a temp dir and
# asserts the script passes with the correct SHA and FAILS with a staled SHA
# (used by CI, which has no Enclave checkout).
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
RECORD_REL="third_party/agent_core/.agent_core-source-sha"

EXPECTED_SHA=""
ENCLAVE_CHECKOUT="${ENCLAVE_CHECKOUT:-}"
SELF_TEST=0

while [ $# -gt 0 ]; do
  case "$1" in
    --expected-sha)
      [ $# -ge 2 ] || { echo "STALE: --expected-sha requires a value" >&2; exit 1; }
      EXPECTED_SHA="$2"
      shift 2
      ;;
    --self-test)
      SELF_TEST=1
      shift
      ;;
    -h|--help)
      sed -n '2,/^set -u/p' "${BASH_SOURCE[0]}" | sed 's/^# \?//'
      exit 0
      ;;
    -*)
      echo "STALE: unknown flag: $1 (see --help)" >&2
      exit 1
      ;;
    *)
      ENCLAVE_CHECKOUT="$1"
      shift
      ;;
  esac
done

# check_one <enclave-checkout> <expected-sha>: the real freshness comparison.
check_one() {
  local checkout="$1"
  local expected="$2"
  local record="$checkout/$RECORD_REL"

  if [ ! -d "$checkout/third_party/agent_core" ]; then
    echo "MISSING: no vendored tree at $checkout/third_party/agent_core" >&2
    echo "  Fix: re-vendor agent_core into the Enclave checkout (see docs/vendoring.md)." >&2
    return 1
  fi
  if [ ! -f "$record" ]; then
    echo "MISSING: no record file at $record" >&2
    echo "  Fix: re-vendor and write .agent_core-source-sha (see docs/vendoring.md)." >&2
    return 1
  fi

  local recorded
  recorded="$(tr -d '[:space:]' < "$record")"
  if [ "$recorded" != "$expected" ]; then
    echo "STALE: vendored copy at $checkout/third_party/agent_core has drifted." >&2
    echo "  recorded SHA: $recorded" >&2
    echo "  expected SHA: $expected" >&2
    echo "  Fix: re-vendor agent_core at $expected (see docs/vendoring.md)." >&2
    return 1
  fi

  echo "FRESH: vendored copy at $checkout/third_party/agent_core matches $expected"
  return 0
}

if [ "$SELF_TEST" -eq 1 ]; then
  SELF_TMP="$(mktemp -d)"
  trap 'rm -rf "$SELF_TMP"' EXIT
  FAKE_ENCLAVE="$SELF_TMP/enclave"
  mkdir -p "$FAKE_ENCLAVE/third_party/agent_core"
  GOOD_SHA="3b5c8df36078546948d27409c4467f3bec60c90b"
  printf '%s\n' "$GOOD_SHA" > "$FAKE_ENCLAVE/$RECORD_REL"

  echo "self-test 1/2: correct SHA must PASS"
  if ! check_one "$FAKE_ENCLAVE" "$GOOD_SHA"; then
    echo "self-test FAIL: correct SHA did not pass" >&2
    exit 1
  fi

  echo "self-test 2/2: staled SHA must FAIL"
  printf '%s\n' "0000000000000000000000000000000000000000" > "$FAKE_ENCLAVE/$RECORD_REL"
  if check_one "$FAKE_ENCLAVE" "$GOOD_SHA"; then
    echo "self-test FAIL: staled SHA passed but must fail" >&2
    exit 1
  fi
  echo "self-test: PASS (fresh passes, stale fails)"
  exit 0
fi

if [ -z "$ENCLAVE_CHECKOUT" ]; then
  ENCLAVE_CHECKOUT="$REPO_ROOT/../enclave"
fi
if [ -z "$EXPECTED_SHA" ]; then
  EXPECTED_SHA="$(git -C "$REPO_ROOT" rev-parse HEAD)"
fi

check_one "$ENCLAVE_CHECKOUT" "$EXPECTED_SHA"
