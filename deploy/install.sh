#!/usr/bin/env bash
# Installs autoreview as the system service of install path b: the system user
# autoreview, the binary, the configuration and state directories, the secrets
# file and the units of deploy/systemd/.
#
# usage: deploy/install.sh --binary PATH [--root DIR]
#
# PATH is the autoreview binary of a checked release archive. A file or
# directory that is in place is not written again, so a second run changes
# nothing. Once /etc/autoreview/config.yaml exists, validate runs as the
# service user with the new binary before the binary or a unit is replaced and
# before any restart. The secrets file is never read: only its owner and mode
# are checked. With --root DIR every path is placed under DIR, and the steps
# that need root (the user, ownership, validate, systemctl) are skipped.
#
# Exit codes: 1 usage, 2 a precondition (root, the binary), 3 the secrets file,
# 4 validate, 5 a failed install step.
set -euo pipefail

USER_NAME=autoreview
CONFIG_DIR=/etc/autoreview
STATE_DIR=/var/lib/autoreview
BIN=/usr/local/bin/autoreview
UNIT_DIR=/etc/systemd/system
UNITS=(autoreview.service autoreview-queue.service autoreview-queue.timer)
VERSION_PATTERN='^autoreview [0-9]+\.[0-9]+\.[0-9]+$'

# Prints `install: <message>` on stderr and exits with code $1.
fail() {
    local code=$1
    shift
    printf 'install: %s\n' "$*" >&2
    exit "$code"
}

# Prints one result line: the item $1 and what happened to it, $2.
report() {
    printf '  %-48s %s\n' "$1" "$2"
}

# Succeeds when $1 equals one of the other arguments.
is_one_of() {
    local item=$1 candidate
    shift
    for candidate in "$@"; do
        [ "$candidate" != "$item" ] || return 0
    done
    return 1
}

usage() {
    fail 1 "usage: deploy/install.sh --binary PATH [--root DIR]"
}

parse_arguments() {
    binary="" root=""
    while [ "$#" -gt 0 ]; do
        case $1 in
            --binary | --root)
                if [ "$#" -lt 2 ] || [ -z "$2" ]; then
                    usage
                fi
                if [ "$1" = --binary ]; then binary=$2; else root=$2; fi
                shift 2
                ;;
            *) usage ;;
        esac
    done
    [ -n "$binary" ] || usage
    if [ -n "$root" ]; then
        [ -d "$root" ] || fail 1 "--root $root is not a directory"
        root=$(cd -- "$root" && pwd -P)
        [ "$root" != / ] || fail 1 "--root / is the system itself; run without --root"
    elif [ "$(id -u)" -ne 0 ]; then
        fail 2 "run as root, or pass --root DIR to install into DIR"
    fi
}

# Checks that $binary is an autoreview release binary and prints its version.
check_binary() {
    if [ ! -f "$binary" ] || [ ! -x "$binary" ]; then
        fail 2 "$binary is not an executable file"
    fi
    local version
    version=$("$binary" --version 2>/dev/null) || fail 2 "$binary --version failed"
    [[ $version =~ $VERSION_PATTERN ]] || fail 2 "$binary is not an autoreview binary"
    echo "installing $version into ${root:-/}"
}

# Creates the system user when it is absent.
step_user() {
    if [ -n "$root" ]; then
        report "user $USER_NAME" "skipped (--root)"
    elif getent passwd "$USER_NAME" >/dev/null; then
        report "user $USER_NAME" "in place"
    else
        useradd --system --user-group --home-dir "$STATE_DIR" --no-create-home \
            --shell /usr/sbin/nologin "$USER_NAME" || fail 5 "useradd $USER_NAME failed"
        report "user $USER_NAME" "created"
    fi
}

# Ensures directory $1 with mode $2, owned by $3 (user:group) unless --root.
ensure_dir() {
    local path=$root$1 mode=$2 owner=$3 want have
    want=$mode
    [ -n "$root" ] || want="$mode $owner"
    if [ -d "$path" ] && [ ! -L "$path" ]; then
        have=$(stat -c '%a' -- "$path")
        [ -n "$root" ] || have="$have $(stat -c '%U:%G' -- "$path")"
        if [ "$have" = "$want" ]; then
            report "$1" "in place"
            return
        fi
    elif [ -e "$path" ] || [ -L "$path" ]; then
        fail 5 "$path is in the way: it is not a directory"
    fi
    install -d -m "$mode" -- "$path" || fail 5 "creating $path failed"
    chmod "$mode" -- "$path" || fail 5 "chmod $mode $path failed"
    [ -n "$root" ] || chown "$owner" -- "$path" || fail 5 "chown $owner $path failed"
    report "$1" "set to $want"
}

# Creates the empty secrets file, or checks the owner and mode of an existing
# one; its content is never read.
step_secrets_file() {
    local path=$root$CONFIG_DIR/autoreview.env owner=$USER_NAME have
    [ -z "$root" ] || owner=$(id -un)
    if [ -e "$path" ] || [ -L "$path" ]; then
        [ -f "$path" ] || fail 3 "$path is not a regular file"
        have=$(stat -L -c '%a %U' -- "$path")
        case $have in
            "600 $owner" | "400 $owner") report "$CONFIG_DIR/autoreview.env" "in place" ;;
            *)
                fail 3 "$path has mode and owner $have; it must be 600 or 400 and" \
                    "owned by $owner: run chown $owner:$owner $path && chmod 0600 $path"
                ;;
        esac
        return
    fi
    if [ -n "$root" ]; then
        install -m 0600 /dev/null "$path" || fail 5 "creating $path failed"
    else
        install -m 0600 -o "$USER_NAME" -g "$USER_NAME" /dev/null "$path" \
            || fail 5 "creating $path failed"
    fi
    report "$CONFIG_DIR/autoreview.env" "created empty, mode 0600"
}

# Copies $binary next to the installed one as .autoreview.install when their
# bytes differ; sets staged to that path, or to empty when nothing changes.
# A copy that an interrupted run left behind is removed first.
stage_binary() {
    local target=$root$BIN
    staged=""
    rm -f -- "${target%/*}/.autoreview.install"
    if [ -f "$target" ] && cmp -s -- "$binary" "$target" \
        && [ "$(stat -c '%a' -- "$target")" = 755 ]; then
        report "$BIN" "in place"
        return
    fi
    install -d -m 0755 -- "${target%/*}" || fail 5 "creating ${target%/*} failed"
    staged=${target%/*}/.autoreview.install
    install -m 0755 -- "$binary" "$staged" || fail 5 "copying $binary failed"
}

# Prints the units whose installed file differs from deploy/systemd/.
changed_units() {
    local name
    for name in "${UNITS[@]}"; do
        cmp -s -- "$source_dir/systemd/$name" "$root$UNIT_DIR/$name" \
            && [ "$(stat -c '%a' -- "$root$UNIT_DIR/$name")" = 644 ] \
            || printf '%s\n' "$name"
    done
}

# Runs validate as the service user with the binary $1, as the service runs it.
# Its JSON error is shown on failure; it names keys and lines, never a value.
step_validate() {
    local output
    if [ -n "$root" ]; then
        report "validate" "skipped (--root)"
        return
    fi
    if ! output=$(systemd-run --wait --pipe --quiet --collect \
        -p "User=$USER_NAME" -p "Group=$USER_NAME" \
        "$1" --config "$CONFIG_DIR/config.yaml" --env-file "$CONFIG_DIR/autoreview.env" \
        validate </dev/null); then
        printf '%s\n' "$output" >&2
        [ -z "$staged" ] || rm -f -- "$staged"
        fail 4 "validate failed; nothing was replaced or restarted"
    fi
    report "validate" "passed"
}

# Moves the staged binary into place and writes the changed units $@.
replace_files() {
    local name target
    if [ -n "$staged" ]; then
        mv -f -- "$staged" "$root$BIN" || fail 5 "replacing $BIN failed"
        report "$BIN" "installed"
    fi
    install -d -m 0755 -- "$root$UNIT_DIR" || fail 5 "creating $UNIT_DIR failed"
    for name in "${UNITS[@]}"; do
        target=$root$UNIT_DIR/$name
        if ! is_one_of "$name" "$@"; then
            report "$UNIT_DIR/$name" "in place"
            continue
        fi
        { install -m 0644 -- "$source_dir/systemd/$name" "$target.install" \
            && mv -f -- "$target.install" "$target"; } || fail 5 "writing $target failed"
        report "$UNIT_DIR/$name" "written"
    done
}

# Enables the service and the timer, and restarts what changed or is stopped.
# $1 is 1 when the binary changed; $2... are the changed units.
step_systemctl() {
    local binary_changed=$1
    shift
    if [ -n "$root" ]; then
        report "systemctl" "skipped (--root)"
        return
    fi
    if [ "$#" -gt 0 ]; then
        systemctl daemon-reload || fail 5 "systemctl daemon-reload failed"
    fi
    if [ "$(systemctl is-enabled autoreview.service 2>/dev/null)" != enabled ] \
        || [ "$(systemctl is-enabled autoreview-queue.timer 2>/dev/null)" != enabled ]; then
        systemctl enable --quiet autoreview.service autoreview-queue.timer \
            || fail 5 "systemctl enable failed"
        report "systemctl enable" "done"
    fi
    if [ "$binary_changed" -eq 1 ] || [ "$#" -gt 0 ] \
        || ! systemctl is-active --quiet autoreview.service; then
        systemctl restart autoreview.service || fail 5 "systemctl restart failed"
        report "autoreview.service" "restarted"
    else
        report "autoreview.service" "running"
    fi
    if [ "$#" -gt 0 ] || ! systemctl is-active --quiet autoreview-queue.timer; then
        systemctl restart autoreview-queue.timer || fail 5 "systemctl restart failed"
        report "autoreview-queue.timer" "restarted"
    fi
}

main() {
    umask 022
    parse_arguments "$@"
    source_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
    check_binary
    step_user
    ensure_dir "$CONFIG_DIR" 755 root:root
    ensure_dir "$STATE_DIR" 700 "$USER_NAME:$USER_NAME"
    step_secrets_file
    stage_binary
    local -a changed
    mapfile -t changed < <(changed_units)
    local configured=0 binary_changed=0
    [ -z "$staged" ] || binary_changed=1
    if [ -f "$root$CONFIG_DIR/config.yaml" ]; then
        configured=1
        step_validate "${staged:-$root$BIN}"
    fi
    replace_files "${changed[@]}"
    if [ "$configured" -eq 0 ]; then
        report "systemctl" "skipped (no $CONFIG_DIR/config.yaml)"
        echo "Next: write $CONFIG_DIR/config.yaml with autoreview init (docs/install.md)," \
            "write the secrets into $CONFIG_DIR/autoreview.env with an editor," \
            "then run this installer again."
        return
    fi
    step_systemctl "$binary_changed" "${changed[@]}"
}

main "$@"
