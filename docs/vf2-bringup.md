<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Wari — VisionFive 2 Bringup

This is the one-time setup to make a VF2 board ready to receive Wari
deploys. After this, every kernel update is `make deploy` on the dev
machine + `wari go` on the VF2.

## Prerequisites
- VF2 booted into Debian Bookworm/Sid (the default StarFive image works)
- Network connectivity (DHCP fine)
- Root SSH or serial access
- USB-UART cable on the GPIO header (for kernel-level visibility on COM7)

## One-time setup on the VF2

```bash
cd /root
git clone https://github.com/westerngazoo/Wari.git wari
cp wari/scripts/wari-upgrade.sh /root/
echo 'source /root/wari-upgrade.sh' >> /root/.bashrc
source /root/.bashrc
wari status                  # smoke-check
```

Expected `wari status` output: build number, last 5 commits, and
`/boot/kernel.bin` size.

## Per-deploy flow

On the dev machine:

```bash
make deploy                  # builds wari.bin, commits, pushes to GitHub
```

On the VF2:

```bash
wari go                      # git pull + cp to /boot/kernel.bin + reboot
```

The board reboots into Wari. Watch the COM7 serial console for the
Phase-0 demo banner:

```
Wari v0 build N boot OK, hart 1
[kvm] heap ...
mmu OK, traps installed
tier-2 uart driver loaded
Hello from Wari
[hello] exit(0)
```

## Recovery: boot back into Debian

If the user kept goose-os's previous kernel as `/boot/kernel.bin.bak`,
restoring it (`cp /boot/kernel.bin.bak /boot/kernel.bin && reboot`) brings
Debian back. Otherwise re-flash the StarFive Debian image to the SD
card. A full recovery doc lands in Phase-1b at `docs/vf2-recovery.md`.

## Subnet caveat

If the dev machine and VF2 are on different subnets (common with DHCP),
direct SSH from dev to VF2 won't work. The deploy flow is intentionally
GitHub-mediated: `make deploy` pushes to GitHub; the VF2 pulls. Both
sides need internet only, no host-to-host routing. As a side benefit,
every flashed kernel is a git commit — R8 reproducibility extends from
the dev machine to the device.
