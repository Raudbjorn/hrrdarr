#!/bin/sh
# Runs a command with HRRDARR_CASEFOLD_DIR set to an empty casefold directory on a private,
# disposable tmpfs (issue #49 real-mount evidence). Nothing outside the command is touched:
#   tests/support/with-casefold-dir.sh cargo test --locked --test rename_commands \
#       real_casefold -- --ignored --test-threads=1 --nocapture
# Needs unprivileged user namespaces, Linux >= 6.13 (tmpfs `casefold`) and chattr. The mount lives
# in a private mount namespace that disappears with the command; no root and no host mount.
# Build first (cargo test --no-run) if you do not want the compiler to run inside the namespace.
set -eu

if [ "$#" -eq 0 ]; then
    echo "usage: $0 command [args...]" >&2
    exit 2
fi

exec unshare --user --map-root-user --mount sh -c '
    set -eu
    base=$(mktemp -d "${TMPDIR:-/tmp}/hrrdarr-casefold.XXXXXX")
    mount -t tmpfs -o casefold,size=64m tmpfs "$base"
    mkdir "$base/library"
    chattr +F "$base/library"
    export HRRDARR_CASEFOLD_DIR="$base/library"
    status=0
    "$@" || status=$?
    cd /
    umount "$base"
    rmdir "$base"
    exit "$status"
' sh "$@"
