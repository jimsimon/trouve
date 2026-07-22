---
name: skill-creator
description: Create or improve a provider-neutral Trouve Agent Skill with clear triggers and workflows.
argument-hint: "<skill goal>"
---

# Skill creator

Create maintainable Agent Skills for Trouve.

1. Clarify the capability, trigger conditions, expected inputs, and success criteria.
2. Place project skills at `.agents/skills/<name>/SKILL.md`; use the user's Trouve config `skills/<name>/SKILL.md` only when the skill should apply across workspaces.
3. Use concise front matter with a stable lowercase `name`, a trigger-oriented `description`, and an optional `argument-hint`.
4. Set `disable-model-invocation: true` for workflows that must only run when explicitly selected, and `user-invocable: false` for model-only background guidance.
5. Keep the main workflow in `SKILL.md`. Make instructions provider-neutral and refer to Trouve tool names and behavior, not a vendor harness.
6. Include safety constraints, decision points, verification, and expected output where they matter. Avoid generic advice the base agent already knows.
7. Test both catalog discovery and the intended explicit or automatic trigger. Check that a workspace skill correctly overrides a built-in or user skill with the same name.
