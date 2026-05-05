# SPDX-License-Identifier: AGPL-3.0-only
# wari-upgrade.sh — Pull latest Wari kernel, verify, and reboot.
#
# Place in /root/ on the VF2, source from .bashrc for the `wari`
# command. Hardened version (May 2026): every stage validates
# before the next runs, so a stale `wari go` cannot silently
# reboot into the wrong build.
#
# Why /boot/kernel.bin (not /boot/wari.bin): keeps U-Boot's
# /boot/extlinux/extlinux.conf unchanged from goose-os days.

WARI_DIR="/root/wari"
WARI_KERNEL="/boot/kernel.bin"

# Extract the build number embedded in a kernel binary by grepping
# for the `WARI-BUILD-TAG-<n>` rodata string. Trustworthy because it
# is what the binary was actually compiled with — `.build_number`
# can drift if cargo's incremental cache misses an env-var change.
# Prints the number, or "?" if the tag is not present.
_wari_embedded_build() {
    local f="$1"
    [ -s "$f" ] || { echo "?"; return; }
    local tag
    tag=$(strings "$f" 2>/dev/null | grep -m1 'WARI-BUILD-TAG-' || true)
    if [ -z "$tag" ]; then
        echo "?(pre-tag-build)"
    else
        echo "${tag#WARI-BUILD-TAG-}"
    fi
}

# Internal: pull origin/main with no merge surprises. Returns:
#   0  + sets BUILD_NUM + EXPECTED_MD5      — repo is at latest
#   1                                        — abort with reason already printed
_wari_pull_and_verify() {
    cd "$WARI_DIR" || { echo "ERROR: $WARI_DIR not found"; return 1; }

    local branch
    branch=$(git rev-parse --abbrev-ref HEAD)
    if [ "$branch" != "main" ]; then
        echo "ERROR: VF2 repo is on branch '$branch', expected 'main'"
        echo "       Fix:  cd $WARI_DIR && git checkout main"
        return 1
    fi

    echo "Fetching origin/main..."
    git fetch origin main 2>&1 | sed 's/^/  /' || {
        echo "ERROR: git fetch failed"
        return 1
    }

    local local_head remote_head
    local_head=$(git rev-parse HEAD)
    remote_head=$(git rev-parse origin/main)
    echo "  local  HEAD:  ${local_head:0:10}"
    echo "  remote HEAD:  ${remote_head:0:10}"

    if [ "$local_head" = "$remote_head" ]; then
        echo "  (already at latest — no pull needed)"
    else
        # Snapshot the wari-upgrade.sh hash before the reset; if the
        # reset brings in a new version of THIS script, we need to
        # re-source it in the caller's shell so the next 'wari ...'
        # invocation uses the new function. Caller signal: we set
        # WARI_SCRIPT_CHANGED=1 and the top-level wari() catches it.
        local script_path="scripts/wari-upgrade.sh"
        local script_before script_after
        script_before=$(md5sum "$script_path" 2>/dev/null | awk '{print $1}')
        echo "Hard-reset to origin/main..."
        git reset --hard origin/main 2>&1 | sed 's/^/  /' || {
            echo "ERROR: git reset failed"
            return 1
        }
        script_after=$(md5sum "$script_path" 2>/dev/null | awk '{print $1}')
        if [ "$script_before" != "$script_after" ]; then
            echo "  >>> $script_path changed in this pull"
            export WARI_SCRIPT_CHANGED=1
        fi
    fi

    if [ ! -s build/wari.bin ]; then
        echo "ERROR: build/wari.bin missing or empty after pull"
        return 1
    fi

    local file_build tree_build
    file_build=$(_wari_embedded_build build/wari.bin)
    tree_build=$(cat .build_number 2>/dev/null || echo "?")
    BUILD_NUM="$file_build"
    EXPECTED_MD5=$(md5sum build/wari.bin | awk '{print $1}')
    local size
    size=$(stat -c%s build/wari.bin)
    echo "  build/wari.bin: embedded-build=$file_build .build_number=$tree_build"
    echo "                  size=$size md5=$EXPECTED_MD5"
    if [ "$file_build" != "$tree_build" ] && [ "$tree_build" != "?" ]; then
        echo "  WARNING: embedded build ($file_build) != .build_number ($tree_build)"
        echo "           This means the producer's cargo incremental cache missed"
        echo "           a WARI_BUILD env-var bump. Push side needs a clean rebuild."
    fi
    return 0
}

# Internal: copy → md5-verify → atomic rename. Returns 0 on full match.
_wari_flash_and_verify() {
    local src="$WARI_DIR/build/wari.bin"
    local staging="${WARI_KERNEL}.staging"

    echo "Flashing to $WARI_KERNEL..."
    cp "$src" "$staging" || { echo "ERROR: cp to $staging failed"; return 1; }
    sync

    local staging_md5
    staging_md5=$(md5sum "$staging" | awk '{print $1}')
    if [ "$staging_md5" != "$EXPECTED_MD5" ]; then
        echo "ERROR: md5 mismatch after copy"
        echo "  expected: $EXPECTED_MD5"
        echo "  staging:  $staging_md5"
        rm -f "$staging"
        return 1
    fi

    mv "$staging" "$WARI_KERNEL" || {
        echo "ERROR: mv $staging -> $WARI_KERNEL failed"
        return 1
    }
    sync

    local flashed_md5
    flashed_md5=$(md5sum "$WARI_KERNEL" | awk '{print $1}')
    if [ "$flashed_md5" != "$EXPECTED_MD5" ]; then
        echo "ERROR: md5 mismatch after flash"
        echo "  expected: $EXPECTED_MD5"
        echo "  flashed:  $flashed_md5"
        return 1
    fi

    echo "  $WARI_KERNEL: build=$BUILD_NUM md5=$flashed_md5  VERIFIED"
    return 0
}

# Visible countdown then reboot. Argument is the starting second
# (e.g. 5 prints "5..4..3..2..1.." then reboots). Each tick goes
# to the same line via \r so the operator can also read what
# build / md5 was just printed before the screen blanks.
_wari_countdown_and_reboot() {
    local n="${1:-5}"
    sync
    while [ "$n" -gt 0 ]; do
        printf "\rRebooting in %d second(s)... " "$n"
        sleep 1
        n=$((n - 1))
    done
    printf "\rRebooting now...                  \n"
    reboot
}

wari() {
    case "${1:-help}" in
        upgrade|up)
            echo "=== Wari Upgrade ==="
            _wari_pull_and_verify || return 1
            # If the pull brought in a new version of this script,
            # re-source it so the next call uses the fresh function.
            # Then transparently re-invoke the same command so the
            # operator never has to remember to source manually.
            if [ "${WARI_SCRIPT_CHANGED:-}" = "1" ]; then
                echo "  re-sourcing scripts/wari-upgrade.sh and continuing..."
                unset WARI_SCRIPT_CHANGED
                # shellcheck source=/dev/null
                source "$WARI_DIR/scripts/wari-upgrade.sh"
                wari "$@"
                return $?
            fi
            _wari_flash_and_verify || return 1
            echo ""
            echo "  Build $BUILD_NUM ready in $WARI_KERNEL"
            echo "  Run 'wari reboot' to boot into it"
            echo ""
            ;;
        go)
            # Upgrade + reboot in one shot, with confirm gate so a
            # bad pull cannot silently reboot into the wrong kernel.
            # Pass -y to skip the confirm.
            local skip_confirm=""
            [ "${2:-}" = "-y" ] && skip_confirm=1
            echo "=== Wari Go ==="
            _wari_pull_and_verify || return 1
            # If the pull brought in a new wari-upgrade.sh, re-source
            # and re-invoke. The operator never has to source by hand.
            if [ "${WARI_SCRIPT_CHANGED:-}" = "1" ]; then
                echo "  re-sourcing scripts/wari-upgrade.sh and continuing..."
                unset WARI_SCRIPT_CHANGED
                # shellcheck source=/dev/null
                source "$WARI_DIR/scripts/wari-upgrade.sh"
                wari "$@"
                return $?
            fi
            _wari_flash_and_verify || return 1
            echo ""
            local flashed_build_after
            flashed_build_after=$(_wari_embedded_build "$WARI_KERNEL")
            echo "========================================"
            echo "  READY TO REBOOT"
            echo "    embedded build:  $flashed_build_after"
            echo "    .build_number:   $BUILD_NUM"
            echo "    md5:             $EXPECTED_MD5"
            echo "========================================"
            if [ "$flashed_build_after" != "$BUILD_NUM" ] && [ "$BUILD_NUM" != "?" ]; then
                echo "WARNING: embedded ($flashed_build_after) != .build_number ($BUILD_NUM)"
                echo "         Push side may have a stale cargo cache."
            fi
            if [ -z "$skip_confirm" ]; then
                read -r -p "Proceed? [y/N] " ans
                case "$ans" in
                    y|Y|yes|YES) ;;
                    *) echo "Aborted. Kernel staged but not rebooted."; return 0 ;;
                esac
            fi
            _wari_countdown_and_reboot 5
            ;;
        reboot|rb)
            # Verify what's flashed is actually our latest before
            # we reboot — protects against a hand-edited /boot.
            if [ ! -s "$WARI_KERNEL" ]; then
                echo "ERROR: $WARI_KERNEL missing or empty"; return 1
            fi
            local build flashed_md5 flashed_build expected_md5
            build=$(cat "$WARI_DIR/.build_number" 2>/dev/null || echo "?")
            flashed_md5=$(md5sum "$WARI_KERNEL" | awk '{print $1}')
            flashed_build=$(_wari_embedded_build "$WARI_KERNEL")
            expected_md5=$(md5sum "$WARI_DIR/build/wari.bin" 2>/dev/null | awk '{print $1}')
            echo "About to reboot into build $build (embedded tag: $flashed_build)"
            echo "  /boot/kernel.bin md5: $flashed_md5"
            echo "  repo  wari.bin   md5: $expected_md5"
            if [ "$flashed_md5" != "$expected_md5" ]; then
                echo "WARNING: flashed kernel != repo wari.bin"
                echo "         Run 'wari upgrade' first, or 'wari go' to upgrade+reboot."
                read -r -p "Reboot anyway? [y/N] " ans
                case "$ans" in y|Y|yes|YES) ;; *) return 0 ;; esac
            fi
            _wari_countdown_and_reboot 5
            ;;
        status|st)
            cd "$WARI_DIR" 2>/dev/null || { echo "ERROR: $WARI_DIR not found"; return 1; }
            local tree_build branch local_head remote_head
            local repo_md5 flashed_md5 repo_build flashed_build
            tree_build=$(cat .build_number 2>/dev/null || echo "?")
            branch=$(git rev-parse --abbrev-ref HEAD)
            local_head=$(git rev-parse HEAD 2>/dev/null || echo "?")
            git fetch origin main 2>/dev/null
            remote_head=$(git rev-parse origin/main 2>/dev/null || echo "?")
            repo_md5=$(md5sum build/wari.bin 2>/dev/null | awk '{print $1}')
            flashed_md5=$(md5sum "$WARI_KERNEL" 2>/dev/null | awk '{print $1}')
            repo_build=$(_wari_embedded_build build/wari.bin)
            flashed_build=$(_wari_embedded_build "$WARI_KERNEL")
            echo "=== Wari Status ==="
            echo "  branch:          $branch"
            echo "  local HEAD:      ${local_head:0:10}"
            echo "  origin/main:     ${remote_head:0:10}"
            if [ "$local_head" != "$remote_head" ]; then
                echo "  >>> repo is BEHIND origin — run 'wari upgrade'"
            fi
            echo ""
            echo "  .build_number (tree):       $tree_build"
            echo "  build/wari.bin    embedded: $repo_build  md5: $repo_md5"
            echo "  /boot/kernel.bin  embedded: $flashed_build  md5: $flashed_md5"
            if [ "$repo_md5" != "$flashed_md5" ]; then
                echo "  >>> flashed kernel does NOT match repo — run 'wari upgrade'"
            fi
            if [ "$repo_build" != "$tree_build" ] && [ "$tree_build" != "?" ] \
                 && [ "$repo_build" != "?(pre-tag-build)" ]; then
                echo "  >>> .build_number ($tree_build) != binary embedded ($repo_build)"
                echo "      The push side has a stale cargo cache."
            fi
            echo ""
            git log --oneline -5
            ;;
        demo)
            # Show me the demo: do not pull. Just sanity-print + reboot.
            wari status
            echo ""
            echo "Rebooting into the currently flashed kernel in 2 s..."
            sleep 2
            reboot
            ;;
        boot-log|log)
            if command -v dmesg >/dev/null 2>&1; then
                echo "=== dmesg (last 40 lines) ==="
                dmesg | tail -40
                echo ""
                echo "  Note: dmesg here is Debian's, not Wari's."
                echo "  For Wari boot output, watch the COM7 serial console."
            else
                echo "dmesg unavailable. Watch the COM7 serial console for Wari boot output."
            fi
            ;;
        help|*)
            echo "Usage: wari <command>"
            echo ""
            echo "  upgrade  (up)  Pull origin/main, verify md5, flash /boot/kernel.bin"
            echo "  go [-y]        Upgrade + confirm + reboot.  -y skips the confirm."
            echo "  reboot   (rb)  Sanity-check flashed md5 vs repo, then reboot."
            echo "  status   (st)  Show repo HEAD vs origin, flashed md5 vs repo md5."
            echo "  demo           Status + reboot (no pull) — for presentations."
            echo "  boot-log (log) Tail Debian dmesg + pointer to COM7 serial."
            echo ""
            ;;
    esac
}
