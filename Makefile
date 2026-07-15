# Wari — build + test + deploy.
#
# Patterned on ../goose-os/Makefile (which reliably produced both
# QEMU-bootable kernels and VF2 hardware images across ~100 builds).
# Build numbers continue monotonically from goose-os (see CLAUDE.md
# §PR Workflow → Build numbering).

# Build ergonomics (R1): prepend the rustup toolchain dir so bare `make`
# always finds the `cargo`/`rustc` that carry the wasm32 + riscv64 targets.
# Homebrew's cargo (no cross targets) otherwise shadows it and breaks the
# build with "can't find crate for `core`". Removes the need to prefix
# every invocation with PATH="$HOME/.cargo/bin:$PATH". See
# docs/parallel-worklist.md §Build refactor.
export PATH := $(HOME)/.cargo/bin:$(PATH)

KERNEL_CRATE := wari-kernel
KERNEL_ELF   := target/riscv64gc-unknown-none-elf/release/wari
KERNEL_BIN   := build/wari.bin
HELLO_WASM   := apps/hello/target/wasm32-unknown-unknown/release/wari_hello.wasm
QEMU         := qemu-system-riscv64
# SLIRP NAT subnet matches the driver's static IP (192.168.50.10/24
# in nic_iface::init). The hostfwd forwards the host's TCP/8080
# explicitly to the guest at 192.168.50.10:7000 — the Phase-1c
# HTTP-demo Tier-1 (apps/hello, bound to port 7000) becomes
# reachable via `curl http://localhost:8080` on the dev machine.
# Host port is 8080 rather than 7000 because macOS's AirTunes /
# AirPlay receiver squats on TCP/7000 by default.
QEMU_ARGS    := -machine virt -nographic -bios default \
                -global virtio-mmio.force-legacy=false \
                -netdev user,id=net0,net=192.168.50.0/24,host=192.168.50.2,hostfwd=tcp::8080-192.168.50.10:7000 \
                -device virtio-net-device,netdev=net0

# llvm-objcopy from Rust toolchain (install with: rustup component add llvm-tools)
OBJCOPY := $(shell find $${HOME}/.rustup -name llvm-objcopy -type f 2>/dev/null | head -1)

# Build-number ratchet — continuous with goose-os.
BUILD_FILE := .build_number
BUILD_NUM  := $(shell cat $(BUILD_FILE) 2>/dev/null || echo 0)
NEXT_BUILD := $(shell echo $$(($(BUILD_NUM) + 1)))

# VF2 deploy target (IP of VisionFive 2 on local network — inherited from goose-os)
VF2_IP ?= 192.168.86.236

# File sets committed by each deploy target
DEPLOY_FILES := $(KERNEL_BIN) kernel/ abi-shared/ wasi/ apps/ drivers/ \
                platform/ scripts/ docs/ Makefile Cargo.toml Cargo.lock \
                rust-toolchain.toml CLAUDE.md README.md .build_number

.PHONY: help build build-hello build-uart-driver sign-uart-driver \
        build-net-driver sign-net-driver build-vf2 build-all \
        test run debug objdump clean \
        kernel-vf2 flash-sd deploy verify \
        test-unit test-integration test-security test-fuzz \
        clippy clippy-kernel fmt check audit

help:
	@echo "Wari — make targets"
	@echo ""
	@echo "  Build:"
	@echo "    build              kernel (qemu target)"
	@echo "    build-hello        Tier-1 hello.wasm"
	@echo "    build-all          kernel + hello + drivers (Phase 1+)"
	@echo "    kernel-vf2         kernel binary for VF2 hardware"
	@echo ""
	@echo "  Run / test:"
	@echo "    run                build + boot in QEMU"
	@echo "    test               timed QEMU boot (Phase 0 smoke)"
	@echo "    debug              QEMU with GDB server :1234"
	@echo "    test-unit          host-side cargo test (pure-logic crates)"
	@echo "    test-integration   QEMU-driven integration tests"
	@echo "    test-security      adversarial WASM tests"
	@echo "    test-fuzz          cargo-fuzz targets (long-running)"
	@echo ""
	@echo "  Quality gates:"
	@echo "    clippy             cargo clippy on host crates, -D warnings"
	@echo "    clippy-kernel      cargo clippy on the kernel (riscv64 target)"
	@echo "    fmt                cargo fmt --check"
	@echo "    check              clippy + fmt + test-unit"
	@echo "    audit              cargo-audit on Cargo.lock"
	@echo ""
	@echo "  Deploy:"
	@echo "    deploy             build + commit + push for VF2"
	@echo "    flash-sd SD=/path  copy kernel.bin to SD card"
	@echo ""
	@echo "  Misc:"
	@echo "    objdump            disassemble kernel .text"
	@echo "    clean              cargo clean"

# ── Build ──────────────────────────────────────────────────────

build: sign-uart-driver sign-net-driver build-hello
	@echo "  [build] kernel: QEMU virt (entry 0x80200000)"
	@cd kernel && WARI_BUILD=$(NEXT_BUILD) cargo build --release --features qemu
	@echo $(NEXT_BUILD) > $(BUILD_FILE)
	@echo "  [build] kernel: build $(NEXT_BUILD), QEMU virt"

# Build the Tier-1 hello.wasm and stage it where the kernel's
# `runtime/hello_blob.rs` `include_bytes!` expects it. Phase 0
# Tier-1 is unsigned (Q4) — no signing step.
build-hello:
	cd apps/hello && cargo build --release
	mkdir -p build/apps
	cp target/wasm32-unknown-unknown/release/wari_hello.wasm \
		build/apps/hello.wasm

# Build per-platform Tier-2 UART driver blobs (PR 9). The kernel
# `include_bytes!`s the platform-matched signed blob — see
# kernel/src/runtime/uart_blob.rs.
build-uart-driver:
	@mkdir -p build/drivers
	@echo "  [build] uart driver: building both platform variants (QEMU + VF2)"
	@echo "          (kernel/src/runtime/uart_blob.rs cfg-selects which one is embedded)"
	@echo "  [build] uart driver: QEMU variant"
	@cd drivers/uart && cargo build --release --features qemu --no-default-features
	@cp target/wasm32-unknown-unknown/release/wari_driver_uart.wasm \
		build/drivers/uart-qemu.wasm
	@echo "  [build] uart driver: VF2 variant"
	@cd drivers/uart && cargo build --release --features vf2 --no-default-features
	@cp target/wasm32-unknown-unknown/release/wari_driver_uart.wasm \
		build/drivers/uart-vf2.wasm

# Sign both Tier-2 UART driver variants. Required before the kernel
# can `include_bytes!` either uart-qemu.signed.wasm (QEMU build) or
# uart-vf2.signed.wasm (VF2 build).
sign-uart-driver: build-uart-driver
	cargo run --manifest-path scripts/Cargo.toml --bin sign-module -- \
	  build/drivers/uart-qemu.wasm build/drivers/uart-qemu.signed.wasm
	cargo run --manifest-path scripts/Cargo.toml --bin sign-module -- \
	  build/drivers/uart-vf2.wasm  build/drivers/uart-vf2.signed.wasm

# Build per-platform Tier-2 net driver blobs (PR Net-4a). The kernel
# `include_bytes!`s the platform-matched signed blob — see
# kernel/src/runtime/net_blob.rs.
build-net-driver:
	@mkdir -p build/drivers
	@echo "  [build] net driver: building both platform variants (QEMU + VF2)"
	@echo "          (kernel/src/runtime/net_blob.rs cfg-selects which one is embedded)"
	@echo "  [build] net driver: QEMU variant (VirtIO-net)"
	@cd drivers/net && WARI_BUILD=$(NEXT_BUILD) \
	  cargo build --release --features qemu --no-default-features
	@cp target/wasm32-unknown-unknown/release/wari_driver_net.wasm \
		build/drivers/net-qemu.wasm
	@echo "  [build] net driver: VF2 variant (JH7110 GMAC1, Phase-1c-12 + net-diag)"
	@cd drivers/net && WARI_BUILD=$(NEXT_BUILD) \
	  cargo build --release --features "vf2 gmac1 net-diag" --no-default-features
	@cp target/wasm32-unknown-unknown/release/wari_driver_net.wasm \
		build/drivers/net-vf2.wasm

# Sign both Tier-2 net driver variants. Required before the kernel
# can `include_bytes!` either net-qemu.signed.wasm or
# net-vf2.signed.wasm.
sign-net-driver: build-net-driver
	cargo run --manifest-path scripts/Cargo.toml --bin sign-module -- \
	  build/drivers/net-qemu.wasm build/drivers/net-qemu.signed.wasm
	cargo run --manifest-path scripts/Cargo.toml --bin sign-module -- \
	  build/drivers/net-vf2.wasm  build/drivers/net-vf2.signed.wasm

# VF2 cross-compile sanity (no flash). Useful before PR 10 deploy.
build-vf2: sign-uart-driver sign-net-driver build-hello
	cd kernel && WARI_BUILD=$(NEXT_BUILD) \
	  cargo build --release --features vf2 --no-default-features

build-all: build build-hello

# ── Run / test ─────────────────────────────────────────────────

run: build
	@echo ">>> Exit QEMU: Ctrl-A then X  (two separate presses)"
	$(QEMU) $(QEMU_ARGS) -kernel $(KERNEL_ELF)

test: build
	timeout 5 $(QEMU) $(QEMU_ARGS) -kernel $(KERNEL_ELF) || true

debug: build
	@echo "Connect: riscv64-linux-gnu-gdb -ex 'target remote :1234' $(KERNEL_ELF)"
	$(QEMU) $(QEMU_ARGS) -kernel $(KERNEL_ELF) -s -S

objdump: build
	rust-objdump -d $(KERNEL_ELF) | head -80

# ── Host-side tests ────────────────────────────────────────────

# Host-testable workspace crates — the pure-logic crates whose test
# profile links std. Explicit `-p` list, NOT --workspace: the kernel
# and the wasm32 crates (drivers/*, apps/*) define their own
# #[panic_handler] and cannot build under the host test harness
# (E0152 + RISC-V asm — see docs/kernel-host-testing-design.md §2),
# and tests/{integration,security} are QEMU-driven with their own
# targets below. Extraction PRs (wari-sched, wari-validate, wari-cap)
# append here as they land. Keep in sync with the duplicate list in
# scripts/build.sh step [1/7] (build.sh cannot depend on make — it
# exists for boxes without it).
HOST_CRATES := -p wari-abi -p wari-driver-iface -p wari-mem \
               -p wari-wnm -p wari-policy -p wari-ipc -p wari-wasi \
               -p wari-cap -p wari-sched -p wari-validate

test-unit:
	cargo test $(HOST_CRATES)

test-integration: build build-hello
	cd tests/integration && cargo test --release

test-security: build build-hello
	cd tests/security && cargo test --release

test-fuzz:
	@echo ">>> Long-running. Use cargo fuzz run <target> for a specific target."
	cd tests/fuzz && cargo fuzz list

# ── Quality gates ──────────────────────────────────────────────

# Two passes: (1) production targets under the full Tier-0 lint wall,
# including clippy::{unwrap,expect,panic}_used per R5; (2) all targets
# (tests, benches) with ONLY those three assertion-style lints allowed
# — test code unwraps and panics by design, everything else stays
# -D warnings. Pass 1 already enforced the strict wall on lib code,
# so pass 2's allowance cannot mask a production violation.
clippy:
	cargo clippy $(HOST_CRATES) -- -D warnings
	cargo clippy $(HOST_CRATES) --all-targets -- -D warnings \
	  -A clippy::unwrap-used -A clippy::expect-used -A clippy::panic

# Kernel lint under the real riscv64 target. Separate from `clippy`
# because it needs the full include_bytes! artifact closure (signed
# driver blobs + hello.wasm) and the WARI_BUILD tag to satisfy
# build.rs's stale-driver guard — so it rides the same dependency
# chain as `build`. Deliberately NOT wired into `check` yet: whether
# the artifact-pipeline cost belongs in the every-PR gate is
# Gustavo's call (see the host-test gate PR discussion).
clippy-kernel: sign-uart-driver sign-net-driver build-hello
	cd kernel && WARI_BUILD=$(NEXT_BUILD) \
	  cargo clippy --release --features qemu --no-default-features -- -D warnings

fmt:
	cargo fmt --all --check

check: fmt clippy test-unit
	@echo ">>> check OK"

audit:
	@which cargo-audit >/dev/null || (echo "install: cargo install cargo-audit" && exit 1)
	cargo audit

# ── VisionFive 2 ───────────────────────────────────────────────

kernel-vf2: sign-uart-driver sign-net-driver build-hello
	@echo "  [build] kernel: VF2 (entry 0x40200000, hart 1)"
	@cd kernel && WARI_BUILD=$(NEXT_BUILD) \
	  cargo build --release --features vf2 --no-default-features
	@$(OBJCOPY) -O binary $(KERNEL_ELF) $(KERNEL_BIN)
	@echo $(NEXT_BUILD) > $(BUILD_FILE)
	@echo "  [build] kernel: build $(NEXT_BUILD), VF2"
	@ls -lh $(KERNEL_BIN)
	@echo ">>> $(KERNEL_BIN) ready — build $(NEXT_BUILD)"

# End-to-end build-coherence check. Greps the embedded WARI-*-BUILD-TAG
# strings out of every artifact and compares them to .build_number.
# If any mismatch → ABORT before flash. This is the operator-visible
# half of the stale-driver guard (kernel/build.rs has the cargo-time
# half). Runs in <1s; cheap to run before every flash.
verify:
	@TREE=$$(cat $(BUILD_FILE) 2>/dev/null || echo "?"); \
	KBIN=$$(strings $(KERNEL_BIN) 2>/dev/null | grep '^WARI-BUILD-TAG-' | head -1 | sed 's/WARI-BUILD-TAG-//' || echo "?"); \
	DVF2=$$(strings build/drivers/net-vf2.signed.wasm 2>/dev/null | grep '^WARI-DRV-BUILD-TAG-' | head -1 | sed 's/WARI-DRV-BUILD-TAG-//' || echo "?"); \
	DQEM=$$(strings build/drivers/net-qemu.signed.wasm 2>/dev/null | grep '^WARI-DRV-BUILD-TAG-' | head -1 | sed 's/WARI-DRV-BUILD-TAG-//' || echo "?"); \
	echo "Build artifact coherence:"; \
	echo "   .build_number ............................... $$TREE"; \
	echo "   $(KERNEL_BIN) embedded ...................... $$KBIN"; \
	echo "   build/drivers/net-vf2.signed.wasm  embedded . $$DVF2"; \
	echo "   build/drivers/net-qemu.signed.wasm embedded . $$DQEM"; \
	echo ""; \
	if [ "$$TREE" = "$$KBIN" ] && [ "$$TREE" = "$$DVF2" ] && [ "$$TREE" = "$$DQEM" ]; then \
	  echo "   OK — all artifacts at build $$TREE"; \
	else \
	  echo "   MISMATCH — run 'make kernel-vf2' to rebuild everything."; \
	  echo "              Bypassing this is how builds 107-114 shipped"; \
	  echo "              a stale driver under a fresh-looking kernel."; \
	  exit 1; \
	fi

# kernel-vf2 with the debug-kernel feature on. Adds kdebug!()
# subsystem-tagged lines to the COM7 trace. Use for diagnostic
# silicon runs only — production deploys use plain `kernel-vf2`.
kernel-vf2-debug: sign-uart-driver sign-net-driver build-hello
	@echo "  [build] kernel: VF2 + debug-kernel feature"
	@cd kernel && WARI_BUILD=$(NEXT_BUILD) \
	  cargo build --release --features vf2,debug-kernel --no-default-features
	@$(OBJCOPY) -O binary $(KERNEL_ELF) $(KERNEL_BIN)
	@echo $(NEXT_BUILD) > $(BUILD_FILE)
	@echo "  [build] kernel: build $(NEXT_BUILD), VF2 (DEBUG)"
	@ls -lh $(KERNEL_BIN)

# kernel-vf2 with both debug-kernel + trace-kernel on. Loudest
# possible silicon trace; expect screens of output per second.
kernel-vf2-trace: sign-uart-driver sign-net-driver build-hello
	@echo "  [build] kernel: VF2 + debug-kernel + trace-kernel"
	@cd kernel && WARI_BUILD=$(NEXT_BUILD) \
	  cargo build --release --features vf2,trace-kernel --no-default-features
	@$(OBJCOPY) -O binary $(KERNEL_ELF) $(KERNEL_BIN)
	@echo $(NEXT_BUILD) > $(BUILD_FILE)
	@echo "  [build] kernel: build $(NEXT_BUILD), VF2 (TRACE)"
	@ls -lh $(KERNEL_BIN)

# One-command deploy: build, commit, push. Matches goose-os flow so
# the VF2 device-side `wari go` script (Phase 1a port) pulls + flashes.
deploy: kernel-vf2
	git add $(DEPLOY_FILES)
	git commit -m "Build $(NEXT_BUILD) (wari deploy)" --allow-empty || true
	git push
	@echo ""
	@echo "========================================="
	@echo "  DEPLOYED: build $(NEXT_BUILD)"
	@echo "========================================="
	@echo "  On the VF2 ($(VF2_IP)), run:"
	@echo "      wari go"
	@echo "  Then watch the COM7 serial console for:"
	@echo "      Wari v0 build $(NEXT_BUILD) boot OK, hart 1"
	@echo "========================================="

flash-sd: kernel-vf2
ifndef SD
	$(error Set SD= to mounted FAT32 partition: make flash-sd SD=/media/goose/boot)
endif
	cp $(KERNEL_BIN) $(SD)/kernel.bin
	sync
	@echo ">>> Copied $(KERNEL_BIN) to $(SD)"

# ── Housekeeping ───────────────────────────────────────────────

clean:
	cargo clean
	rm -f $(KERNEL_BIN)
