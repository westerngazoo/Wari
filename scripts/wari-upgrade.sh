# SPDX-License-Identifier: AGPL-3.0-only
# wari-upgrade.sh — Pull latest Wari kernel and reboot into it.
#
# Place in /root/ on the VF2, source from .bashrc for the `wari`
# command. Cherry-picked from goose-os/scripts/goose-upgrade.sh
# (proven across ~100 device flashes); branding + paths adapted
# for Wari, plus a `demo` and `boot-log` subcommand.
#
# Why /boot/kernel.bin (not /boot/wari.bin): keeps U-Boot's
# /boot/extlinux/extlinux.conf unchanged from goose-os days. A
# Phase-1b PR will introduce a U-Boot menu for dual-boot.

WARI_DIR="/root/wari"

wari() {
    case "${1:-help}" in
        upgrade|up)
            echo "=== Wari Upgrade ==="
            cd "$WARI_DIR" || { echo "ERROR: $WARI_DIR not found"; return 1; }
            echo "Pulling latest..."
            git pull || { echo "ERROR: git pull failed"; return 1; }
            local build=$(cat .build_number 2>/dev/null || echo "?")
            echo "Copying wari.bin (build $build) to /boot/kernel.bin..."
            cp build/wari.bin /boot/kernel.bin || { echo "ERROR: cp failed"; return 1; }
            echo ""
            echo "  Build $build ready in /boot/kernel.bin"
            echo "  Run 'wari reboot' to boot into it"
            echo ""
            ;;
        go)
            # upgrade + reboot in one shot — the everyday flow
            wari upgrade && wari reboot
            ;;
        reboot|rb)
            echo "Rebooting into Wari..."
            sleep 1
            reboot
            ;;
        status|st)
            cd "$WARI_DIR" 2>/dev/null || { echo "ERROR: $WARI_DIR not found"; return 1; }
            local build=$(cat .build_number 2>/dev/null || echo "?")
            echo "Wari OS build: $build"
            echo "Repo: $WARI_DIR"
            git log --oneline -5
            echo ""
            ls -lh /boot/kernel.bin 2>/dev/null || echo "/boot/kernel.bin not found"
            ;;
        demo)
            # Single-keystroke "show me the demo" flow: status, then
            # reboot into the latest deployed kernel. Distinct from
            # `wari go` because it does NOT pull — the operator wants
            # to demo what's currently on disk, not race a deploy.
            wari status
            echo ""
            echo "Rebooting into the demo build..."
            sleep 2
            reboot
            ;;
        boot-log|log)
            # Tail Debian's dmesg as a "did we get past banner?" hint
            # FROM Debian. If Wari halted before Debian could see
            # anything (i.e. the device rebooted into Wari and stayed
            # there), dmesg here belongs to a previous Debian boot —
            # the COM7 serial console is the source of truth.
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
            echo "  upgrade  (up)  Pull latest kernel and copy to /boot/kernel.bin"
            echo "  go             Upgrade + reboot in one shot (everyday flow)"
            echo "  reboot   (rb)  Reboot into Wari now"
            echo "  status   (st)  Show current build info"
            echo "  demo           Status + reboot (no pull) — for presentations"
            echo "  boot-log (log) Tail Debian dmesg + pointer to COM7 serial"
            echo ""
            ;;
    esac
}
