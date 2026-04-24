# Wari — Engineering Principles

> These four principles sit alongside the Code Quality Standards in
> `CLAUDE.md` and the PR workflow in `pr-workflow.md`. They govern how
> every line of code in Wari gets written, by humans and by AI
> collaborators alike. Cite them by number in PR bodies when they
> apply.

---

## 1. Think Before Coding

**Don't assume. Don't hide confusion. Surface trade-offs.**

LLMs (and rushed humans) often pick an interpretation silently and
run with it. This principle forces explicit reasoning:

- **State assumptions explicitly** — If uncertain, ask rather than
  guess.
- **Present multiple interpretations** — Don't pick silently when
  ambiguity exists.
- **Push back when warranted** — If a simpler approach exists, say
  so. Rule #6 of the co-architect protocol obliges this: Claude
  surfaces disagreement in writing before executing.
- **Stop when confused** — Name what's unclear and ask for
  clarification. Do not proceed with a compile-clean guess.

Concrete application in a PR body: when the `Why` or `How` section
encountered a fork in the road, the four-question depth rule from
`pr-workflow.md` covers one direction — document the alternatives
considered. When the fork could not be resolved without Gustavo's
input, quote the chat decision verbatim or link to it.

---

## 2. Simplicity First

**Minimum code that solves the problem. Nothing speculative.**

Combat the tendency toward over-engineering:

- No features beyond what was asked.
- No abstractions for single-use code. (Cross-reference:
  `CLAUDE.md` §Code Quality #4 — "Every abstraction pays for
  itself.")
- No "flexibility" or "configurability" that wasn't requested.
- No error handling for impossible scenarios.
- If 200 lines could be 50, rewrite it.

**The test**: Would a senior engineer say this is overcomplicated?
If yes, simplify.

Concrete application: when PR 1 adds a new module, every
`pub fn`, every trait, every type parameter has to earn its place.
A trait with one impl is debt; a function extracted "for future
reuse" is debt; a generic parameter with one instantiation is debt.

---

## 3. Surgical Changes

**Touch only what you must. Clean up only your own mess.**

When editing existing code:

- **Don't "improve" adjacent code**, comments, or formatting.
- **Don't refactor things that aren't broken**.
- **Match existing style**, even if you'd do it differently.
- **If you notice unrelated dead code, mention it — don't delete
  it.** It may belong to in-flight work you can't see.

When your changes create orphans:

- Remove imports / variables / functions that **your** changes made
  unused.
- Don't remove **pre-existing** dead code unless asked.

**The test**: Every changed line should trace directly to the user's
request.

This principle is the operational form of `CLAUDE.md` §Co-Architect
rule #3 ("No silent structural changes") and rule #5 ("Tactical
cleanup during a task is allowed — structural changes are not
tactical").

---

## 4. Goal-Driven Execution

**Define success criteria. Loop until verified.**

Transform imperative tasks into verifiable goals:

| Instead of… | Transform to… |
|---|---|
| "Add validation" | "Write tests for invalid inputs, then make them pass" |
| "Fix the bug" | "Write a test that reproduces it, then make it pass" |
| "Refactor X" | "Ensure tests pass before and after" |
| "Make it faster" | "Measure baseline, change code, verify target threshold is met" |
| "Clean up the driver" | (not a goal — reject until re-scoped) |

For multi-step tasks, state a brief plan before executing:

```
1. [Step] → verify: [check]
2. [Step] → verify: [check]
3. [Step] → verify: [check]
```

Strong success criteria let the AI (or any executor) loop
independently. Weak criteria ("make it work") require constant
clarification.

Concrete application for Wari: every PR has a **Local verification**
section (per `pr-workflow.md`). That section is the reification of
this principle — it's the exact checks that verify success, with
expected outputs. If the verification commands are vague, the PR
cannot be evaluated. If they're specific, the reviewer can run them
and know the moment the PR is done.

Phase-level goal-driven execution: every phase gate has numbered,
testable exit criteria (see `CLAUDE.md` §Phase 0 Exit Criteria for
the template). A phase isn't done when someone feels it's done;
it's done when all N criteria have a green verification trail.

---

## Cross-references

These principles are the *how*; the *what* they govern lives in:

- `CLAUDE.md` §Code Quality Standards — the six per-module rules
- `CLAUDE.md` §Absolute Rules (R1–R8) — the kernel invariants
- `CLAUDE.md` §Co-Architect Protocol — the human-AI decision loop
- `pr-workflow.md` §Why/How depth rule — per-decision documentation
- `testing.md` §What "coverage" means — the success-criteria
  standard

When these principles conflict with any specific rule (e.g.,
Simplicity First vs. a required feature flag), the specific rule
wins. When the specific rules are silent, these principles apply.
