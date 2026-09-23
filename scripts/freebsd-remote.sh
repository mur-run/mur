#!/bin/sh
# Drive a self-hosted FreeBSD 15.1 host from a GitHub Actions linux runner.
#
# Why SSH and not a self-hosted GitHub runner process: the official
# actions/runner has no FreeBSD build, so `runs-on: [self-hosted, freebsd]`
# cannot execute Node-based actions (actions/checkout, upload-artifact) on
# the host. The Linux job keeps the Node actions; only plain commands cross
# the SSH boundary. The FreeBSD host therefore needs no runner agent, no
# Node, and no GitHub credential.
#
# Usage:
#   FREEBSD_HOST=user@host FREEBSD_SSH_KEY=/path/to/key \
#     scripts/freebsd-remote.sh <remote-command...>
#
# Optional:
#   FREEBSD_SSH_PORT     default 22
#   FREEBSD_REMOTE_DIR   default /tmp/mur-ci/$GITHUB_RUN_ID-$GITHUB_RUN_ATTEMPT
#   FREEBSD_FETCH        space-separated remote paths copied back into $PWD
#
# The working tree is shipped as a git archive of HEAD, so the remote build
# sees exactly the committed tree and nothing from the runner's cache.
set -eu

: "${FREEBSD_HOST:?FREEBSD_HOST is required (user@host)}"
: "${FREEBSD_SSH_KEY:?FREEBSD_SSH_KEY is required (path to private key)}"

PORT=${FREEBSD_SSH_PORT:-22}
RUN_ID=${GITHUB_RUN_ID:-local}
RUN_ATTEMPT=${GITHUB_RUN_ATTEMPT:-1}
REMOTE_DIR=${FREEBSD_REMOTE_DIR:-/tmp/mur-ci/${RUN_ID}-${RUN_ATTEMPT}}
FETCH=${FREEBSD_FETCH:-}

if [ "$#" -eq 0 ]; then
    printf 'freebsd-remote: no remote command given\n' >&2
    exit 2
fi

SSH="ssh -p $PORT -i $FREEBSD_SSH_KEY \
-o BatchMode=yes \
-o StrictHostKeyChecking=yes \
-o ConnectTimeout=30 \
-o ServerAliveInterval=30 \
-o ServerAliveCountMax=10"

cleanup() {
    status=$?
    # Never let cleanup mask the real exit code.
    $SSH "$FREEBSD_HOST" "rm -rf '$REMOTE_DIR'" >/dev/null 2>&1 || true
    exit "$status"
}
trap cleanup EXIT HUP INT TERM

printf 'freebsd-remote: host=%s dir=%s\n' "$FREEBSD_HOST" "$REMOTE_DIR"

# Fail fast and loudly when the host is not the platform we claim to test.
$SSH "$FREEBSD_HOST" 'uname -s' | grep -qx FreeBSD || {
    printf 'freebsd-remote: remote host is not FreeBSD\n' >&2
    exit 1
}

$SSH "$FREEBSD_HOST" "mkdir -p '$REMOTE_DIR'"
git archive --format=tar HEAD |
    $SSH "$FREEBSD_HOST" "tar -xf - -C '$REMOTE_DIR'"

set +e
$SSH "$FREEBSD_HOST" "cd '$REMOTE_DIR' && $*"
remote_status=$?
set -e

for path in $FETCH; do
    # Artifacts are evidence: fetch them even when the command failed.
    scp -P "$PORT" -i "$FREEBSD_SSH_KEY" -o BatchMode=yes \
        "$FREEBSD_HOST:$REMOTE_DIR/$path" . 2>/dev/null ||
        printf 'freebsd-remote: missing artifact %s\n' "$path" >&2
done

exit "$remote_status"
