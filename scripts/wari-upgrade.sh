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
    echo "  local  HEAD: ${local_head:0:10}"
    echo "  remote HEAD: ${remote_head:0:10}"

    if [ "$local_head" = "$remote_head" ]; then
        echo "  (already at latest — no pull needed)"
    else
        echo "Hard-reset to origin/main..."
        git reset --hard origin/main 2>&1 | sed 's/^/  /' || {
            echo "ERROR: git reset failed"
            return 1
        }
    fi

    if [ ! -s build/wari.bin ]; then
        echo "ERROR: build/wari.bin missing or empty after pull"
        return 1
    fi

    BUILD_NUM=$(cat .build_number 2>/dev/null || echo "?")
    EXPECTED_MD5=$(md5sum build/wari.bin | awk '{print $1}')
    local size
    size=$(stat -c%s build/wari.bin)
    echo "  build/wari.bin: build=$BUILD_NUM size=$size md5=$EXPECTED_MD5"
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

wari() {
    case "${1:-help}" in
        upgrade|up)
            echo "=== Wari Upgrade ==="
            _wari_pull_and_verify || return 1
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
            _wari_flash_and_verify || return 1
            echo ""
            echo "========================================"
            echo "  READY TO REBOOT into build $BUILD_NUM"
            echo "  md5: $EXPECTED_MD5"
            echo "========================================"
            if [ -z "$skip_confirm" ]; then
                read -r -p "Proceed? [y/N] " ans
                case "$ans" in
                    y|Y|yes|YES) ;;
                    *) echo "Aborted. Kernel staged but not rebooted."; return 0 ;;
                esac
            fi
            echo "Rebooting in 2 s..."
            sleep 2
            reboot
            ;;
        reboot|rb)
            # Verify what's flashed is actually our latest before
            # we reboot — protects against a hand-edited /boot.
            if [ ! -s "$WARI_KERNEL" ]; then
                echo "ERROR: $WARI_KERNEL missing or empty"; return 1
            fi
            local build flashed_md5 expected_md5
            build=$(cat "$WARI_DIR/.build_number" 2>/dev/null || echo "?")
            flashed_md5=$(md5sum "$WARI_KERNEL" | awk '{print $1}')
            expected_md5=$(md5sum "$WARI_DIR/build/wari.bin" 2>/dev/null | awk '{print $1}')
            echo "About to reboot into build $build"
            echo "  /boot/kernel.bin md5: $flashed_md5"
            echo "  repo  wari.bin   md5: $expected_md5"
            if [ "$flashed_md5" != "$expected_md5" ]; then
                echo "WARNING: flashed kernel != repo wari.bin"
                echo "         Run 'wari upgrade' first, or 'wari go' to upgrade+reboot."
                read -r -p "Reboot anyway? [y/N] " ans
                case "$ans" in y|Y|yes|YES) ;; *) return 0 ;; esac
            fi
            sync; sleep 1; reboot
            ;;
        status|st)
            cd "$WARI_DIR" 2>/dev/null || { echo "ERROR: $WARI_DIR not found"; return 1; }
            local build branch local_head remote_head flashed_md5 repo_md5
            build=$(cat .build_number 2>/dev/null || echo "?")
            branch=$(git rev-parse --abbrev-ref HEAD)
            local_head=$(git rev-parse HEAD 2>/dev/null || echo "?")
            git fetch origin main 2>/dev/null
            remote_head=$(git rev-parse origin/main 2>/dev/null || echo "?")
            repo_md5=$(md5sum build/wari.bin 2>/dev/null | awk '{print $1}')
            flashed_md5=$(md5sum "$WARI_KERNEL" 2>/dev/null | awk '{print $1}')
            echo "=== Wari Status ==="
            echo "  repo build #:    $build"
            echo "  branch:          $branch"
            echo "  local HEAD:      ${local_head:0:10}"
            echo "  origin/main:     ${remote_head:0:10}"
            if [ "$local_head" != "$remote_head" ]; then
                echo "  >>> repo is BEHIND origin — run 'wari upgrade'"
            fi
            echo ""
            echo "  repo  wari.bin   md5: $repo_md5"
            echo "  /boot/kernel.bin md5: $flashed_md5"
            if [ "$repo_md5" != "$flashed_md5" ]; then
                echo "  >>> flashed kernel does NOT match repo — run 'wari upgrade'"
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
