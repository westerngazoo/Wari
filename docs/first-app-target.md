<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Wari — First App Target: a Capability-Confined, AI-Native Cloud Workspace

> **Status:** Target, recorded 2026-09-27. Gustavo's direction; this
> document is the annotation plus the options-with-reasoning the
> Co-Architect Protocol asks for. Nothing here is scheduled yet.
>
> **One sentence:** an Emacs-class extensible editor whose extension
> language is **Tier-1 WASM**, whose workspace is **distributed across
> Wari nodes**, and whose AI agent is a **capability-confined Tier-2
> module** rather than a process holding the user's ambient authority.

Companion to [`ai-os-assistant-design.md`](ai-os-assistant-design.md)
(the agent's confinement — this app is its first real consumer),
[`cap-system-design.md`](cap-system-design.md),
[`architecture.md`](architecture.md), and the roadmap in `CLAUDE.md`.

---

## 1 · Why an editor, and why this one

The target is aggressive on purpose. The argument for it is not "an
editor would be nice" — it is that **this specific app is the
architecture's own proof**, and it is the only app on the table whose
requirements are already the roadmap's requirements.

**Emacs's defining property is not its keybindings.** It is that the
editor is written in its own extension language, so every behavior is
inspectable and redefinable while the system runs. Emacs bought that
with a catastrophic trade: zero isolation. Any elisp package you install
runs with your full authority and can read your SSH keys. Forty years
on, nobody has fixed this, because the substrate cannot fix it.

Wari's Tier-1 WASM **is** that substrate, with the trade reversed:

| Emacs property | Wari mechanism | What changes |
|---|---|---|
| Extensions in the system's own language | Extension = Tier-1 WASM capsule | Any language that targets wasm32 |
| Redefine behavior while running | Dynamic module lifecycle (p2a) | Hot-load without dropping buffers |
| Extensions have full user authority | Extension holds a **capability set** | A linter cannot read your keys |
| One address space, one crash kills all | Validator + Sv39 per instance | A bad extension dies alone |
| Single machine | Distributed caps (p3d) | Workspace spans nodes |

That combination — **Emacs's extensibility with actual isolation** — is
not a reskin of an existing product. It is the thing the two-tier model
was built to make possible, and no existing editor can retrofit it,
because it is a property of the OS underneath, not of the editor.

## 2 · What "native-first AI" has to mean here

Everyone is shipping an AI editor. The differentiator is not the model
and not the UX; it is **what the agent is allowed to do when it is
wrong**.

Per [`ai-os-assistant-design.md`](ai-os-assistant-design.md) §1, prompt
injection is structural, not a model-quality bug. So the claim this app
makes is a *security* claim:

> A fully prompt-injected agent in this editor still cannot exceed the
> capability set it was granted, because a deterministic non-LLM
> authority (`wari-policy`) mediates every effect.

Cursor, Copilot and VSCode's agents structurally cannot make that claim:
their agent is a process running as you, holding your ambient authority,
and the only thing between a poisoned README and your credentials is the
model's judgment. Wari's answer is the Planner/Executor split that
already exists as a design and a merged crate — the editor is its first
real workload, not a new mechanism.

This is also the **sovereignty** argument made concrete: an institution
that cannot ship its source to a US SaaS gets an AI development
environment on its own silicon, where the agent's blast radius is a
capability set rather than a user account.

## 3 · Shape: thin client, workspace on Wari

Wari has no display stack and no GPU driver, and should not grow one for
this. That constraint points at the right architecture rather than away
from it — the same one VSCode Remote and Codespaces converged on:

```
  ┌──────────────┐        ┌────────────────────────────────────────┐
  │ thin client  │        │            WARI NODE(S)                │
  │ (browser or  │◀──────▶│  Tier 1: buffers · modes · extensions  │
  │  terminal)   │  wire  │  Tier 2: agent (Planner/Executor)      │
  │  keys+pixels │        │  Tier 2: net, fs · Tier 0: kernel      │
  └──────────────┘        └────────────────────────────────────────┘
```

The client renders and captures input. **Everything that is the editor**
— buffers, modes, extensions, LSP, the agent — lives on Wari as WASM.
The "distributed env" is then a property of the workspace, not of the
client: buffers and extension instances are addressable across nodes via
distributed capabilities (p3d), so a workspace is a *capability graph*,
not a machine.

## 4 · What it depends on (all already on the roadmap)

The target adds **no new kernel concepts**. It consumes scheduled work,
which is the strongest evidence it is the right demo:

| Need | Roadmap brick | State |
|---|---|---|
| Install/hot-reload an extension | p2a dynamic module lifecycle | Phase 2 spine, open |
| The wire | p2b `wari-http` | Phase 2, open |
| Agent tool calls, mediated | p2e/p2f MCP + Executor | Phase 2b, designed |
| Typing latency | AOT (G-track) | **Hard dependency, see §5** |
| Workspace across nodes | p3a multikernel, p3d distributed caps | Phase 3 |
| Per-tenant confinement | cap system | **Done, on silicon** |

## 5 · Where I push back

Recording the target is right. Three things should be recorded honestly
alongside it:

1. **Scope is the real risk.** "Emacs + VSCode + distributed + AI" is
   four products, and Emacs alone is forty years and ~1.5M lines. The
   target is a *north star*, not a backlog item. Its value today is as a
   **forcing function**: it decides Phase 2 design questions that are
   otherwise arguable. ("Must the module lifecycle support hot-reload
   without dropping state?" — the editor says yes, so that is now a
   requirement with a reason attached.)

2. **AOT stops being a parallel lane and becomes a dependency.** An
   editor is the most latency-sensitive app class there is: the
   keystroke→echo budget is ~16 ms and ideally under 8. `wasmi` is a
   pure interpreter. An interpreted editor core will not meet that. This
   target promotes the G-track from "nice, in parallel" to "on the
   critical path," and that promotion should be explicit rather than
   discovered later.

3. **The distributed half is genuinely Phase 3+.** It needs multikernel
   and distributed capabilities, neither of which exists. Any near-term
   milestone should be single-node and say so.

## 6 · Proposed A0 — the smallest thing that proves the thesis

Simplicity First applied to a large vision: do **not** start by building
an editor. Build the smallest artifact that demonstrates the claim no
competitor can make, and let it be ugly.

**A0: "the confined extension."** A Tier-1 WASM buffer holding text, a
thin client that can type into it over the wire, **one** extension
hot-loaded at runtime that transforms the buffer, and **one** AI edit
arriving through the Executor and denied by `wari-policy` when it
reaches outside its capability set.

Success is not "it edits text." Success is the **denial**: a demo where
the agent is asked to exfiltrate something and the OS — not the model,
not a prompt — stops it. That single screenshot is the entire product
thesis, and it is reachable on Phase 2 bricks without multikernel, a
display stack, or anything Emacs-shaped.

## 7 · Prior art

Per CLAUDE.md rule 7, what this builds on and what it rejects:

| Pattern | Source | Use |
|---|---|---|
| Editor as a live, self-redefining environment | Emacs (Stallman & Steele, 1976); Lisp Machines; Smalltalk-80 | Inherit the extensibility thesis, reject the zero-isolation trade |
| Thin frontend, workspace on a remote host | VSCode Remote / Codespaces / Gitpod (2019–) | Inherit the split directly |
| Editor ↔ tooling decoupling | LSP (Microsoft, 2016) | Extensions and language servers are separate WASM capsules |
| Per-process namespaces as the unit of a distributed environment | Plan 9 (Bell Labs, 1992) | Closest prior art to a capability-scoped workspace; a Wari workspace is Plan 9's namespace with caps as the naming authority |
| Convergent distributed editing | CRDTs (Shapiro et al., 2011) | Candidate primitive for multi-node buffers |
| Mediating an LLM's effects with a non-LLM authority | CaMeL (Google, 2025); seL4 confused deputy (Hardy, 1988) | Already the basis of `ai-os-assistant-design.md` |
| **Rejected:** Electron / a browser per editor | VSCode's delivery model | Exactly the density model Wari rejects; the client is thin, the editor is WASM |

## 8 · Open decisions (Gustavo's call)

1. **Name.** Project tradition is Andean. `CLAUDE.md`'s own etymology
   cites *quipu* — the knotted-cord information system: distributed,
   encoded, addressable records. For a distributed workspace of buffers
   that is almost too apt. **Quipu** is my proposal; *Khipukamayuq* (the
   quipu-keeper) is the alternative if the editor rather than the
   workspace should carry the name.
2. **Client surface first** — terminal (fast, proves the wire) or
   browser (the real target, more work)?
3. **Does A0 land before or after the Phase 2 spine?** It can be built
   as a forcing function *during* p2a, or as its demo *after*.
4. **Is the AOT promotion to critical path accepted** (§5.2)?
