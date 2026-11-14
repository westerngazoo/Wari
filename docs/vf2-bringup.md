<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Wari — VisionFive 2 Bringup

This is the one-time setup to make a VF2 board ready to receive Wari
deploys. After this, every kernel update is `scripts/wari-build.sh
<branch>` on the dev machine and `wari go --build <branch>.<N>` on the VF2. The build side is documented in `docs/build-workflow.md`.

## Prerequisites
- VF2 booted into Debian Bookworm/Sid (the default StarFive image works)
- Network connectivity (DHCP fine)
- Root SSH or serial access
- USB-UART cable on the GPIO header (for kernel-level visibility on COM7)

## One-time setup on the VF2

```bash
cd /root
git clone https://github.com/westerngazoo/Wari.git wari
echo 'source /root/wari/scripts/wari-upgrade.sh' >> /root/.bashrc
source /root/.bashrc
wari status                  # smoke-check
```

Source the script from the repo, never from a frozen copy: when a pull
ships a new `wari-upgrade.sh`, `wari go` re-sources it and re-runs
under the new logic.

Expected `wari status` output: branch, local HEAD vs `origin/main`,
the embedded build tag and md5 of both `build/wari.bin` and
`/boot/kernel.bin`, and the last 5 commits.

## Per-deploy flow

On the dev machine:

```bash
bash scripts/wari-build.sh <branch>   # mirror to GitHub, build, publish; prints N
```

On the VF2:

```bash
wari go --build <branch>.<N>   # download build N's release, verify, flash, reboot
```

The board reboots into Wari. Watch the COM7 serial console for the
Phase-0 demo banner, and check that `build N` is the build you just
published:

```
Wari v0 build N boot OK, hart 1
[kvm] heap ...
mmu OK, traps installed
tier-2 uart driver loaded
Hello from Wari
[hello] exit(0)
```

## Getting into Debian

U-Boot's default `bootcmd` boots Wari (`/boot/kernel.bin`), so an
unattended power-on always lands in Wari. To reach Debian, which is
where `wari go --build` runs:

1. Reset the board (Ctrl-R on the Wari console, the reset button, or a
   power cycle) with the serial console open at 115200.
2. Press any key while U-Boot counts down (`Hit any key to stop
   autoboot`) to get the `StarFive #` prompt.
3. Boot Debian through U-Boot's distro boot:

   ```
   run bootcmd_distro
   ```

The next reset without interrupting the countdown boots Wari again.

## Recovery: boot back into Debian

If the user kept goose-os's previous kernel as `/boot/kernel.bin.bak`,
restoring it (`cp /boot/kernel.bin.bak /boot/kernel.bin && reboot`) brings
Debian back. Otherwise re-flash the StarFive Debian image to the SD
card. A dedicated recovery doc has not been written yet.

## Subnet caveat

If the dev machine and VF2 are on different subnets (common with DHCP),
direct SSH from dev to VF2 won't work. The deploy flow is intentionally
GitHub-mediated: the dev machine pushes the branch and publishes the
release; the VF2 pulls the branch and downloads the binary. Both sides
need internet only, no host-to-host routing. As a side benefit, every
flashed kernel maps to a git commit and a release tag — R8
reproducibility extends from the dev machine to the device.
