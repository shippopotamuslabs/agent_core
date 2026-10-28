# Enclave vendoring contract (agent_core side)

`enclave/third_party/agent_core` is a **synced copy (snapshot)** of this
repository — not a fork, not a source of truth. This repo (`agent_core`) is
the source of truth; the Enclave tree carries a read-only mirror so the
Android build can compile without a second checkout.

## The record file

Every re-vendor writes exactly one file at the root of the vendored copy:

`enclave/third_party/agent_core/.agent_core-source-sha`

Format: a single line containing the full 40-hex-char `agent_core` commit
SHA the snapshot was taken from, terminated by a newline. No other content.
Example:

```text
3b5c8df36078546948d27409c4467f3bec60c90b
```

Template for a re-vendor step (run inside the Enclave checkout):

```bash
# From the agent_core checkout at the commit being vendored:
SRC_SHA="$(git -C /path/to/agent_core rev-parse HEAD)"
# After syncing the tree (rsync/git-archive, excluding .git):
printf '%s\n' "$SRC_SHA" > third_party/agent_core/.agent_core-source-sha
git add third_party/agent_core
```

## Re-vendor procedure (run on the Enclave side)

1. Pick the `agent_core` commit to vendor (normally the tip of the `agent_core`
   release or stack being consumed).
2. Sync the vendored tree from that commit, excluding `.git/` (e.g.
   `git -C /path/to/agent_core archive <sha> | tar -x -C third_party/agent_core`).
3. Write `.agent_core-source-sha` as above with that exact SHA.
4. Commit both the tree and the record file together in the Enclave repo.

The re-vendor itself plus any drift-closing work that needs the Enclave repo
belongs to ENC-294 / Sean — **do not** do it from this repo.

## Freshness check (runnable from this repo)

`scripts/check-vendored-freshness.sh` compares the record file in a given
Enclave checkout against an expected `agent_core` SHA (default: this repo's
`HEAD`):

```bash
# Default Enclave checkout location: ../enclave
./scripts/check-vendored-freshness.sh
# Explicit path, or via env:
./scripts/check-vendored-freshness.sh /path/to/enclave
ENCLAVE_CHECKOUT=/path/to/enclave ./scripts/check-vendored-freshness.sh
# Check against a specific SHA instead of HEAD:
./scripts/check-vendored-freshness.sh --expected-sha <40-hex> /path/to/enclave
```

Exit 0 with `FRESH: ...` on match. Exit 1 with `STALE: ...` / `MISSING: ...`
on mismatch, missing record file, or missing vendored tree — the message
names the file, the expected SHA, and the re-vendor step that fixes it.

## CI

`check-vendored-freshness.sh --self-test` simulates a vendored copy in a temp
dir: it verifies the script passes with the correct SHA and fails loudly with
a staled SHA. The `vendoring-self-test` CI job runs that mode, so it needs no
Enclave checkout. A real Enclave-side check (run the script with the Enclave
checkout present after each re-vendor, failing the build on `STALE`) is
documented here for Sean/ENC-294 rather than added to this repo's CI, because
this repo's CI has no Enclave checkout.
