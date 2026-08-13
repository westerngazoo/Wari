---
sidebar_position: 20
sidebar_label: "Ch 20: Build, Embed, Flash"
title: "Chapter 20 — Build, Embed, Flash"
---

# Chapter 20 — Build, Embed, Flash

A Tier-2 driver is not a file the kernel opens at runtime. It is bytes
*baked into the kernel image* — `include_bytes!` resolves at
kernel-compile time, so by the time the kernel boots, the signed driver
is already part of its `.rodata`. That single fact reorders everything.
There is no "install the driver" step, no version negotiation on the
device, no filesystem lookup. The driver's fate is sealed when the kernel
links. Which means the *build* is where every driver bug either gets
caught or gets shipped.

This chapter walks the pipeline end to end — build the WASM twice, sign
each, embed the platform-matched blob, verify the tags agree, publish —
and then tells the story of the week Wari spent debugging code the kernel
was not even running, because the one gap this pipeline now closes was
still open.

## The pipeline, one command

Every artifact the kernel embeds is produced by one script,
`scripts/build.sh`, invoked with a profile. There is no partial mode and
no à-la-carte build; the script exists precisely because Wari's worst
build incidents all had the same shape — the build had separable steps,
and a human sequenced them wrong (`build.sh:5`). The seven steps, in
order (`build.sh` step markers):

```
1. host unit tests   pure-logic crates — the gate before anything builds
2. Tier-1 programs   apps/<name> → build/apps/<name>.wasm
3. UART driver       both platforms → sign          → *.signed.wasm
4. net driver        both platforms → sign          → *.signed.wasm
5. kernel            include_bytes! the cfg blob     → wari.bin
6. verify            embedded build tags all match   → gate
7. archive + release pointer / optional publish
```

Steps 3 and 4 are where Chapter 18's "built twice" becomes concrete. The
net driver is compiled once for VF2 and once for QEMU, then each is signed
(`build.sh:156`):

```sh
( cd drivers/net && WARI_BUILD=$NEXT_BUILD \
    cargo build --release --features "$DRV_FEATURES" --no-default-features \
    --target wasm32-unknown-unknown )
cp target/wasm32-unknown-unknown/release/wari_driver_net.wasm build/drivers/net-vf2.wasm
( cd drivers/net && WARI_BUILD=$NEXT_BUILD \
    cargo build --release --features qemu --no-default-features \
    --target wasm32-unknown-unknown )
cp target/wasm32-unknown-unknown/release/wari_driver_net.wasm build/drivers/net-qemu.wasm
cargo run … --bin sign-module -- build/drivers/net-vf2.wasm  build/drivers/net-vf2.signed.wasm
cargo run … --bin sign-module -- build/drivers/net-qemu.wasm build/drivers/net-qemu.signed.wasm
```

Notice three disciplines the script enforces, spelled out in its header
(`build.sh:52`). **I1**: the same `WARI_BUILD` env is threaded through the
driver build *and* the kernel build in one invocation, so the two cannot
drift across invocations. **I2**: both platform variants are rebuilt
every run, so the four-way tag verify in step 6 is always meaningful.
**I3**: `.build_number` advances only *after* verify passes. These are not
belt-and-suspenders; each maps to a real incident the script's header
names by build number.

The kernel then embeds exactly one of the two signed blobs, chosen by the
same cargo feature that chose the driver's platform. The switch is a
two-line cfg gate in `kernel/src/runtime/net_blob.rs`:

```rust
#[cfg(feature = "qemu")]
pub static NET_DRIVER_SIGNED: &[u8] =
    include_bytes!("../../../build/drivers/net-qemu.signed.wasm");

#[cfg(feature = "vf2")]
pub static NET_DRIVER_SIGNED: &[u8] =
    include_bytes!("../../../build/drivers/net-vf2.signed.wasm");
```

The UART driver has its twin, `uart_blob.rs`. Both blobs sit in
`build/drivers/` after the sign step; cargo's feature unification
guarantees exactly one of `qemu`/`vf2` is active, so exactly one
`include_bytes!` compiles. The QEMU kernel ELF carries the QEMU-flavoured
signed blob and the VF2 kernel ELF carries the VF2 one, and the mismatched
combination is unrepresentable. Chapter 16 calls this the third of "three
independent guards, same invariant."

## The verify gate

Step 6 (`build.sh:181`) is where the build refuses to lie about itself.
It extracts the embedded build tag from the kernel binary and from *both*
signed driver blobs, and demands all three equal the build number it is
about to stamp:

```sh
KBIN="$(strings build/wari.bin | grep '^WARI-BUILD-TAG-' | head -1 | sed 's/WARI-BUILD-TAG-//')"
DVF2="$(strings build/drivers/net-vf2.signed.wasm | grep '^WARI-DRV-BUILD-TAG-' | ... )"
DQEM="$(strings build/drivers/net-qemu.signed.wasm | grep '^WARI-DRV-BUILD-TAG-' | ... )"
if [ "$KBIN" != "$NEXT_BUILD" ] || [ "$DVF2" != "$NEXT_BUILD" ] || [ "$DQEM" != "$NEXT_BUILD" ]; then
    echo "!! TAG MISMATCH — artifacts incoherent, .build_number NOT advanced"
    exit 1
fi
```

The driver's build tag is a string it plants in its own `.rodata`,
`drivers/net/src/lib.rs:79`:

```rust
#[used]
#[no_mangle]
pub static WARI_DRV_BUILD_TAG: &[u8] =
    concat!("WARI-DRV-BUILD-TAG-", env!("WARI_BUILD"),).as_bytes();
```

`env!("WARI_BUILD")` reads the number the script exported; `#[used]` keeps
LTO from stripping the symbol even though nothing references it, so
`strings(1)` can still find it. The kernel plants an analogous
`WARI-BUILD-TAG-N`. If all three agree, the artifacts are coherent and
the script writes the new number to `.build_number`. If they do not, the
build fails loudly and the number does not advance — an incoherent build
never gets a name.

## The stale-driver guard, and the week of dead code

The verify gate is the belt. The `kernel/build.rs` guard is the
suspenders, and it exists because of the single most instructive failure
in Wari's history.

The kernel's `build.rs` runs a `check_driver_blob_freshness`
(`kernel/build.rs:66`) before the kernel links. It reads the signed
driver blob the kernel is about to embed, greps it for
`WARI-DRV-BUILD-TAG-`, parses the number, and compares it to the kernel's
own `WARI_BUILD` (`build.rs:87`):

```rust
let needle = b"WARI-DRV-BUILD-TAG-";
let pos = bytes.windows(needle.len()).position(|w| w == needle);
// … parse the trailing digits into `got` …
if got != want {
    println!(
        "cargo::error=stale-driver-guard: embedded driver build {} != WARI_BUILD {}. \
         The signed driver wasm at {} is stale. Run `make kernel-vf2` … \
         Never run `cd kernel && cargo build` directly; cargo will happily \
         reuse the last-known-good blob and the bug we're trying to fix \
         will never reach silicon.",
        got, want, blob
    );
}
```

Here is why every word of that error message is earned. In build 107, a
`core::arch::asm!("fence ow,ow")` — a RISC-V CPU instruction — went into
driver code. The driver compiles to `wasm32-unknown-unknown`, where
inline assembly is not supported, so the WASM build *silently failed*.
Cargo did what cargo does: it kept the last artifact that had compiled,
`wari_driver_net.wasm` from build 106. Meanwhile `cd kernel && cargo
build` relinked the kernel happily, embedding the build-106 blob under a
banner that now read "build 114."

For a week — builds 107 through 114 — every diagnostic added to the
driver was a no-op, because the kernel was not running the updated driver.
The symptom, recorded in `docs/diagnostic-tags.md:157`, was maddeningly
specific: the kernel banner read "build 114," but new diagnostic tags
added to `drivers/net/src/lib.rs` never appeared on the console. The team
was reading a boot trace from code that no longer existed.

The fix (build 116) was the build-tag string plus this guard. If the
guard ever fires, you bypassed the pipeline — the remedy is to run
`scripts/build.sh` (or `make`), which rebuilds the driver to wasm32
*before* linking the kernel, not to "fix" `build.rs`. This is also why
Rule 1 from Chapter 18 forbids inline `asm!` so absolutely: the failure
mode is not a loud compile error, it is a *silent* substitution of stale
code, and silence is the expensive kind of wrong.

Two sibling incidents rounded out the case for a single build entrypoint,
both named in the script's header (`build.sh:9`): builds 122–124, where
parallel-dev deploys bumped the kernel while the signed driver stayed a
build behind and the kernel referenced driver exports that were not there;
and builds 130–134, a hand-run eight-step pipeline on a box without
`make`, where every invocation risked a skipped sign or a mismatched
`WARI_BUILD`. `scripts/build.sh` is the answer to all three: one
entrypoint, the full closure of everything the kernel embeds, the
four-way tag verify, no partial mode. The guard caught a real stale-blob
attempt during build 138's development, so it earns its keep.

## The release and pointer flow

Once the artifacts are coherent, they have to reach the board — and this
is where Wari made a deliberate concession to parallel development.
`build/wari.bin` is *no longer tracked by git* (`build.sh:222`): a binary
artifact in git made every parallel branch conflict on an unmergeable
file. Instead the repo tracks `build/wari.release`, a one-line text
pointer naming the GitHub Release tag that carries this build's binaries:

```sh
REL_TAG="build-${NEXT_BUILD}-${BRANCH_SLUG}"
echo "$REL_TAG" > build/wari.release
```

Run with `--publish` and the script cuts the actual release
(`build.sh:243`), uploading `wari.bin` and both signed net-driver blobs
with a notes file carrying their sha256:

```sh
gh release create "$REL_TAG" \
    build/wari.bin build/drivers/net-vf2.signed.wasm build/drivers/net-qemu.signed.wasm \
    --title "Build $NEXT_BUILD ($PROFILE, $BRANCH_SLUG)" --notes-file /tmp/wari-release-notes.txt
```

On the board, the device-side `wari go` downloads the binary named by the
pointer and verifies its embedded `WARI-BUILD-TAG` before flashing — the
same tag the build stamped, checked one last time at the point of no
return. The operator flow, from `docs/net-driver-vf2.md:203`:

```bash
# Dev machine
scripts/build.sh release --publish
git add build/wari.release .build_number && git commit && git push

# On the VF2 (Debian side)
wari upgrade && wari go -y        # flash main
wari go-branch <branch>           # or flash a testing branch
```

A per-branch, per-profile archive lands under
`build/out/<branch-slug>/<profile>/` with a `build-info.txt` recording
number, profile, branch, sha, and features — uncommitted, for local
disambiguation when several branches are in flight. The canonical
`build/wari.bin` is what the pointer resolves to and what the kernel's own
tag verifies.

## Closing hook

You now have the whole loop: a driver is source in a separate crate,
compiled twice, signed twice, embedded once per platform, verified four
ways, and pointed at from a release the board pulls and re-verifies before
it flashes. Every gate in that loop was installed after something slipped
through the gap it now closes.

But a pipeline that reliably ships the right bytes only guarantees you are
debugging *live* code. It says nothing about whether that code is
*correct* — and correctness, on real silicon, against a PHY that answers
at the wrong address through a clock that is not running, wired to timing
margins measured in hundreds of picoseconds, is a different kind of hard.
Chapter 21 is the payoff: the GMAC bring-up as a detective story, three
faults that each disproved the others, and what it actually feels like to
read a hardware failure through a keyhole of tag words.
