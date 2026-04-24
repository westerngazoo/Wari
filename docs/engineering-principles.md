# Wari — Engineering Principles

> Four principles that govern how every line of code in Wari gets
> written. They sit alongside the per-module rules in `CLAUDE.md`
> and the PR workflow in `pr-workflow.md`. Cite them by number in
> PR bodies and code review when they apply.

These are universal. They apply to Wari work regardless of whether
the contributor is a senior engineer, a junior, an external auditor,
or an automated agent. They predate this project — they're distilled
from years of debugging shipped systems — and they will outlast it.

---

## 1. Think Before Coding

**Don't assume. Don't hide confusion. Surface trade-offs.**

The single most common cause of a regression is silent commitment to
one interpretation of an ambiguous spec. The fix is process, not
talent:

- **State assumptions explicitly.** Write down what you believe the
  spec means before writing the code that depends on it. If a
  reviewer can't see your assumption, they can't catch a wrong one.
- **Present multiple interpretations.** When the spec or code allows
  two readings, name both, choose one with reasoning, and link to
  that reasoning from the commit message.
- **Push back when warranted.** If a simpler approach exists, say
  so before writing the more complex one. "I was told to" is not a
  valid post-hoc defense for over-engineered code.
- **Stop when confused.** Name what's unclear and ask. A guess that
  compiles is still a guess.

In Wari this principle is the day-to-day form of the Why/How depth
rule in `pr-workflow.md`: every non-obvious decision in a PR body
answers what was picked, what was considered, why this won, and what
cost was accepted.

---

## 2. Simplicity First

**Minimum code that solves the problem. Nothing speculative.**

The Wari kernel target size is 5–10 KLOC for a reason: a small
kernel can be formally verified; a sprawling one cannot. Every line
either earns its place or makes verification harder.

- No features beyond what was asked.
- No abstractions for single-use code. (See per-module rule #4 in
  `CLAUDE.md`: *"every abstraction pays for itself"*.)
- No "flexibility" or "configurability" that isn't currently required.
- No error handling for cases that cannot occur.
- If 200 lines could be 50, rewrite it.

**The senior-engineer test**: would a senior engineer skimming this
PR call it overcomplicated? If yes, simplify before requesting review.

A trait with one impl is debt. A function extracted "for future
reuse" that never gets reused is debt. A generic parameter with one
instantiation is debt. Indirection is added when the second caller
arrives, not in anticipation of one.

---

## 3. Surgical Changes

**Touch only what you must. Clean up only your own mess.**

Most merge conflicts and regressions come from PRs that quietly
"improve" code adjacent to their stated scope. The discipline:

- Don't reformat code, comments, or imports that aren't part of the
  task.
- Don't refactor things that aren't broken.
- Match existing style, even if you'd do it differently.
- If you notice unrelated dead code, mention it — don't delete it.
  It may belong to in-flight work you can't see.

When your changes legitimately create orphans:

- Remove the imports / variables / functions that **your** changes
  made unused.
- Don't remove **pre-existing** dead code unless explicitly asked.

**The trace test**: every changed line in your diff should trace
directly to the user-visible task or to an orphan you created. If it
doesn't, it's scope creep — split it into its own PR.

This principle is the operational form of the rules in `CLAUDE.md`'s
co-architect protocol: structural changes (refactors, renames,
dependency additions, module moves) are not tactical, even if they
look small.

---

## 4. Goal-Driven Execution

**Define success criteria. Loop until verified.**

A task is finished when its success criteria are met — not when the
contributor feels done. The discipline starts at task definition
time:

| Vague task | Becomes |
|---|---|
| "Add validation" | "Write tests for invalid inputs, then make them pass" |
| "Fix the bug" | "Write a test that reproduces it, then make it pass" |
| "Refactor X" | "Ensure tests pass before and after" |
| "Make it faster" | "Measure baseline, change code, verify the target threshold" |
| "Clean up the driver" | (not a goal — reject until re-scoped) |

For multi-step work, write the plan as numbered steps with a verify
clause each:

```
1. [step] → verify: [check]
2. [step] → verify: [check]
3. [step] → verify: [check]
```

Strong success criteria let any executor loop independently to
completion. Weak criteria require constant clarification and produce
ambiguous "is it done?" reviews.

In Wari every PR has a **Local verification** section in its body
(see `pr-workflow.md`) that lists the exact commands a reviewer
runs to confirm the PR is done. If those commands are vague, the PR
isn't reviewable. If they're specific, the reviewer can run them
and know the moment the work is complete.

Phase-level goal-driven execution: every Wari phase has numbered,
testable exit criteria (see `CLAUDE.md` §Phase 0 Exit Criteria for
the template). A phase isn't done when someone feels it's done;
it's done when every numbered criterion has a green verification
trail.

---

## When principles conflict with rules

These four principles state the *how*; the specific rules in
`CLAUDE.md` (R1–R8 absolute rules, the per-module standards, the
co-architect protocol) state the *what* and the *boundaries*.

When a principle and a specific rule conflict, the specific rule
wins. Example: Simplicity First says "no error handling for cases
that cannot occur," but R5 (no panics in kernel paths) requires a
typed `KernelError` return even when a fault feels impossible. The
rule wins because it encodes a concrete safety property; the
principle yields.

When the specific rules are silent, these principles apply.
