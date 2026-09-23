#!/bin/sh
# Secret-free native FreeBSD 15.1 poudriere feasibility probe.
set -eu

REPORT=${REPORT:-freebsd-spike-report.txt}
EXPECTED_ARCH=${EXPECTED_ARCH:-}
JAIL_PREFIX=${JAIL_PREFIX:-mur151}
PORTS_TREE=${PORTS_TREE:-murports}
POUDRIERE_DATA=${POUDRIERE_DATA:-/usr/local/poudriere}
FAILED_STEP=initialization
START_EPOCH=$(date +%s)
RESULT=failed

: >"$REPORT"
exec 3>>"$REPORT"

report() {
    printf '%s\n' "$*" >&3
}

run() {
    FAILED_STEP=$1
    shift
    report "command[$FAILED_STEP]=$*"
    "$@" >>"$REPORT" 2>&1
}

finish() {
    status=$?
    end_epoch=$(date +%s)
    report "duration_seconds=$((end_epoch - START_EPOCH))"
    report "result=$RESULT"
    if [ "$RESULT" != pass ]; then
        report "failed_step=$FAILED_STEP"
    fi
    df -k >>"$REPORT" 2>&1 || true
    if [ "$status" -ne 0 ]; then
        cat "$REPORT"
    fi
    exit "$status"
}
trap finish EXIT HUP INT TERM

report "started_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
run freebsd-version freebsd-version -ku
run uname uname -m
run identity id
run disk-before df -h

HOST_VERSION=$(freebsd-version -u | sed 's/-p[0-9][0-9]*$//')
case "$HOST_VERSION" in
    15.1-RELEASE) ;;
    *) report "host_version_error=expected 15.1-RELEASE, got $HOST_VERSION"; exit 1 ;;
esac

HOST_ARCH=$(uname -m)
case "$HOST_ARCH" in
    amd64|arm64) ;;
    *) report "host_arch_error=unsupported $HOST_ARCH"; exit 1 ;;
esac
if [ -n "$EXPECTED_ARCH" ] && [ "$HOST_ARCH" != "$EXPECTED_ARCH" ]; then
    report "host_arch_error=expected $EXPECTED_ARCH, got $HOST_ARCH"
    exit 1
fi
report "host_arch=$HOST_ARCH"

FAILED_STEP=zpool-status
if command -v zpool >/dev/null 2>&1 && zpool status >>"$REPORT" 2>&1; then
    ZPOOL_NAME=$(zpool list -H -o name 2>/dev/null | sed -n '1p')
    if [ -n "$ZPOOL_NAME" ]; then
        STORAGE=zfs
        report "storage=zfs"
        report "zpool=$ZPOOL_NAME"
    else
        STORAGE=ufs
        report "storage=ufs"
        report "zpool_status=unavailable (no imported pool)"
    fi
else
    STORAGE=ufs
    report "storage=ufs"
    report "zpool_status=unavailable"
fi

run pkg-repositories pkg -vv
run pkg-bootstrap env ASSUME_ALWAYS_YES=yes pkg bootstrap -f
run install-prerequisites pkg install -y poudriere git ca_root_nss
run package-versions pkg info poudriere git ca_root_nss
run poudriere-version poudriere version

FAILED_STEP=configure-poudriere
mkdir -p /usr/local/etc "$POUDRIERE_DATA"
CONF=/usr/local/etc/poudriere.conf
if [ "$STORAGE" = zfs ]; then
    {
        printf 'ZPOOL=%s\n' "$ZPOOL_NAME"
        printf 'ZROOTFS=/poudriere\n'
        printf 'FREEBSD_HOST=https://download.FreeBSD.org\n'
        printf 'RESOLV_CONF=/etc/resolv.conf\n'
    } >"$CONF"
else
    {
        printf 'NO_ZFS=yes\n'
        printf 'BASEFS=%s\n' "$POUDRIERE_DATA"
        printf 'FREEBSD_HOST=https://download.FreeBSD.org\n'
        printf 'RESOLV_CONF=/etc/resolv.conf\n'
    } >"$CONF"
fi
report "poudriere_conf_begin"
sed -E 's/(TOKEN|PASSWORD|SECRET)=.*/\1=[redacted]/' "$CONF" >&3
report "poudriere_conf_end"

JAIL_NAME="${JAIL_PREFIX}-${HOST_ARCH}"
run delete-old-jail poudriere jail -d -j "$JAIL_NAME" || true
run create-jail poudriere jail -c -j "$JAIL_NAME" -v 15.1-RELEASE -a "$HOST_ARCH"
run delete-old-ports-tree poudriere ports -d -p "$PORTS_TREE" || true
run create-ports-tree poudriere ports -c -p "$PORTS_TREE" -m git+https
run clean-bulk poudriere bulk -c -j "$JAIL_NAME" -p "$PORTS_TREE" ports-mgmt/pkg
run jail-list poudriere jail -l
run ports-list poudriere ports -l
run disk-after df -h

RESULT=pass
report "jail=$JAIL_NAME"
report "ports_tree=$PORTS_TREE"
report "clean_bulk=pass"
