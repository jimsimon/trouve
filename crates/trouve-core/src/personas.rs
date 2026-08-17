//! Data-driven agent personas (invariant 6): a persona is a prompt, a tool
//! policy, and model/permission defaults. Built-ins ship as data; users add
//! or override personas with TOML files in `<config>/personas/` or a
//! workspace's `.agents/personas/`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use trouve_protocol::{AgentPersona, PersonaInfo};

pub const REVIEW_PERSONA_ID: &str = "review";

pub fn is_valid_persona_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

pub fn review_inspection_tools() -> Vec<String> {
    [
        "read_file",
        "list_dir",
        "glob",
        "grep",
        "search",
        "find_related",
        "git_diff",
        "web_fetch",
        "todo_write",
        "spawn_thread",
        "spawn_output",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

pub fn builtin_personas() -> Vec<AgentPersona> {
    vec![
        AgentPersona {
            id: "code".into(),
            display_name: "Engineer".into(),
            system_prompt: "You are the Engineer persona: implement the user's request by editing \
                            files in the workspace. Prefer small verifiable steps; run tests \
                            or builds when they exist. Report what you changed when done."
                .into(),
            allowed_tools: vec![],
            read_only: false,
            default_permission_mode: None,
            default_model: None,
            default_thinking_level: None,
        },
        AgentPersona {
            id: "plan".into(),
            display_name: "Planner".into(),
            system_prompt:
                "You are the Planner persona: explore the workspace and produce a concrete \
                            implementation plan. Do not modify any files; your deliverable is \
                            the plan itself."
                    .into(),
            allowed_tools: vec![
                "read_file".into(),
                "list_dir".into(),
                "glob".into(),
                "grep".into(),
                "search".into(),
                "find_related".into(),
                "git_diff".into(),
                // Codex full-bridge turns disable native network access.
                // Keep legitimate read-only research available through the
                // permission-gated ToolExecutor path.
                "web_fetch".into(),
                "todo_write".into(),
                // Delegation is orchestration rather than a worktree
                // mutation. Read-only children may fan out only into the
                // same read-only persona; `handle_spawn_tool` enforces that
                // invariant and intentionally withholds `spawn_session`.
                "spawn_thread".into(),
                "spawn_output".into(),
            ],
            read_only: true,
            default_permission_mode: None,
            default_model: None,
            default_thinking_level: None,
        },
        AgentPersona {
            id: REVIEW_PERSONA_ID.into(),
            display_name: "Reviewer".into(),
            system_prompt:
                "You are the Reviewer persona: examine the changes in this workspace and \
                            report problems — bugs, missed edge cases, style violations — with \
                            file and line references. Do not modify files."
                    .into(),
            // No shell here: review is read_only, and the gate denies every
            // mutating tool (shell included) in read-only personas, so listing
            // them would only tempt the model into a guaranteed-deny loop.
            allowed_tools: review_inspection_tools(),
            read_only: true,
            default_permission_mode: None,
            default_model: None,
            // Unattended review fans out across focused agents, but the final
            // coordinator also inherits this setting. Medium keeps that
            // adjudication reliable while explicit persona settings can
            // still move narrower work up or down.
            default_thinking_level: Some("medium".into()),
        },
        AgentPersona {
            id: "architect".into(),
            display_name: "Architect".into(),
            system_prompt:
                "You are the Architect persona: reason about structure, boundaries, and \
                            trade-offs. Review designs and changes for maintainability, duplicated \
                            sources of truth, and violated boundaries. Propose designs and ADR-style \
                            records rather than direct code changes."
                    .into(),
            allowed_tools: vec![],
            read_only: false,
            default_permission_mode: None,
            default_model: None,
            default_thinking_level: None,
        },
        AgentPersona {
            id: "question".into(),
            display_name: "Researcher".into(),
            system_prompt: "You are the Researcher persona: answer questions about the workspace. \
                            Read whatever you need; never modify anything."
                .into(),
            allowed_tools: vec![
                "read_file".into(),
                "list_dir".into(),
                "glob".into(),
                "grep".into(),
                "search".into(),
                "find_related".into(),
                "web_fetch".into(),
                "todo_write".into(),
                "spawn_thread".into(),
                "spawn_output".into(),
            ],
            read_only: true,
            default_permission_mode: None,
            default_model: None,
            default_thinking_level: None,
        },
    ]
}

/// The persona to run when a thread references one that no longer resolves
/// (its TOML was deleted or became invalid). Locked down and read-only: a
/// thread the user believed was restricted must not silently gain write
/// access by falling back to the permissive `code` persona.
pub fn fallback_persona() -> AgentPersona {
    AgentPersona {
        id: "restricted".into(),
        display_name: "Restricted".into(),
        system_prompt: "The configured persona is unavailable. Operating in a restricted, \
                        read-only persona: inspect the workspace and report, but do not modify \
                        anything."
            .into(),
        allowed_tools: vec![
            "read_file".into(),
            "list_dir".into(),
            "glob".into(),
            "grep".into(),
            "search".into(),
            "find_related".into(),
            "web_fetch".into(),
            "todo_write".into(),
        ],
        read_only: true,
        default_permission_mode: None,
        default_model: None,
        default_thinking_level: None,
    }
}

fn load_dir(dir: &Path, personas: &mut Vec<AgentPersona>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        match toml::from_str::<AgentPersona>(&text) {
            Ok(persona) => {
                // Later layers override earlier ones by id.
                personas.retain(|m| m.id != persona.id);
                personas.push(persona);
            }
            Err(e) => tracing::warn!("ignoring invalid persona file {}: {e}", path.display()),
        }
    }
}

/// Built-ins, overlaid by `<config>/personas/*.toml`, overlaid by the
/// workspace's `.agents/personas/*.toml`.
pub fn resolve_personas(
    config_dir: Option<&Path>,
    workspace_root: Option<&Path>,
) -> Vec<AgentPersona> {
    let mut personas = builtin_personas();
    if let Some(dir) = config_dir {
        load_dir(&dir.join("personas"), &mut personas);
    }
    if let Some(root) = workspace_root {
        load_dir(&root.join(".agents").join("personas"), &mut personas);
    }
    personas
}

pub fn find_persona<'a>(personas: &'a [AgentPersona], id: &str) -> Option<&'a AgentPersona> {
    personas.iter().find(|m| m.id == id)
}

/// Personas with provenance for the settings UI. Same layering as
/// [`resolve_personas`]; each entry is tagged with where its effective
/// definition came from.
pub fn resolve_persona_infos(
    config_dir: Option<&Path>,
    workspace_root: Option<&Path>,
) -> Vec<PersonaInfo> {
    let builtin_ids: Vec<String> = builtin_personas().iter().map(|m| m.id.clone()).collect();
    let mut infos: Vec<PersonaInfo> = builtin_personas()
        .into_iter()
        .map(|persona| PersonaInfo {
            persona,
            origin: "builtin".into(),
        })
        .collect();
    let mut overlay = |dir: &Path, origin_over_builtin: &str, origin_new: &str| {
        let mut personas = Vec::new();
        load_dir(dir, &mut personas);
        for persona in personas {
            let origin = if builtin_ids.contains(&persona.id) {
                origin_over_builtin.to_string()
            } else {
                origin_new.to_string()
            };
            infos.retain(|i| i.persona.id != persona.id);
            infos.push(PersonaInfo { persona, origin });
        }
    };
    if let Some(dir) = config_dir {
        overlay(&dir.join("personas"), "customized", "custom");
    }
    if let Some(root) = workspace_root {
        let dir = root.join(".agents").join("personas");
        overlay(&dir, "workspace", "workspace");
    }
    // Stable order: built-ins first in their canonical order, then the rest
    // alphabetically.
    infos.sort_by_key(|i| {
        (
            builtin_ids
                .iter()
                .position(|id| *id == i.persona.id)
                .unwrap_or(usize::MAX),
            i.persona.id.clone(),
        )
    });
    infos
}

/// The user-level persona file defining `id`, if any. Prefers `<id>.toml` but
/// falls back to scanning (files may be named freely).
pub(crate) fn user_persona_file(config_dir: &Path, id: &str) -> Option<PathBuf> {
    let dir = config_dir.join("personas");
    let canonical = dir.join(format!("{id}.toml"));
    if canonical.exists() {
        return Some(canonical);
    }
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(persona) = toml::from_str::<AgentPersona>(&text)
            && persona.id == id
        {
            return Some(path);
        }
    }
    None
}

/// Write (create or replace) the user-level TOML file for a persona. Saving
/// under a built-in id customizes that built-in.
pub fn upsert_user_persona(config_dir: &Path, persona: &AgentPersona) -> Result<()> {
    if !is_valid_persona_id(&persona.id) {
        bail!("persona id must be non-empty and [a-zA-Z0-9_-] only");
    }
    let path = user_persona_file(config_dir, &persona.id).unwrap_or_else(|| {
        config_dir
            .join("personas")
            .join(format!("{}.toml", persona.id))
    });
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let text = toml::to_string_pretty(persona).context("serializing persona")?;
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Remove the user-level file for a persona: deletes a custom persona outright,
/// or resets a customized built-in back to its defaults.
pub fn delete_user_persona(config_dir: &Path, id: &str) -> Result<()> {
    let Some(path) = user_persona_file(config_dir, id) else {
        if builtin_personas().iter().any(|m| m.id == id) {
            bail!("persona '{id}' is a built-in with no user override to remove");
        }
        bail!("no user-level persona '{id}'");
    };
    std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_persona_names_describe_person_roles() {
        let personas = builtin_personas();
        for (id, display_name) in [
            ("code", "Engineer"),
            ("plan", "Planner"),
            ("review", "Reviewer"),
            ("architect", "Architect"),
            ("question", "Researcher"),
        ] {
            assert_eq!(
                find_persona(&personas, id).unwrap().display_name,
                display_name
            );
        }
    }

    #[test]
    fn review_persona_defaults_to_medium_thinking_without_changing_plan_persona() {
        let personas = builtin_personas();
        assert_eq!(
            find_persona(&personas, "review")
                .unwrap()
                .default_thinking_level
                .as_deref(),
            Some("medium")
        );
        assert!(
            find_persona(&personas, "plan")
                .unwrap()
                .default_thinking_level
                .is_none()
        );
    }

    #[test]
    fn read_only_builtin_personas_can_delegate_without_spawning_sessions() {
        let personas = builtin_personas();
        for id in ["plan", "review", "question"] {
            let persona = find_persona(&personas, id).unwrap();
            assert!(persona.read_only);
            assert!(persona.allowed_tools.iter().any(|tool| tool == "web_fetch"));
            assert!(
                persona
                    .allowed_tools
                    .iter()
                    .any(|tool| tool == "spawn_thread")
            );
            assert!(
                persona
                    .allowed_tools
                    .iter()
                    .any(|tool| tool == "spawn_output")
            );
            assert!(
                !persona
                    .allowed_tools
                    .iter()
                    .any(|tool| tool == "spawn_session")
            );
        }
    }

    #[test]
    fn workspace_persona_overrides_builtin() {
        let tmp = tempfile::tempdir().unwrap();
        let personas_dir = tmp.path().join(".agents/personas");
        std::fs::create_dir_all(&personas_dir).unwrap();
        std::fs::write(
            personas_dir.join("plan.toml"),
            r#"
id = "plan"
display_name = "Custom Plan"
system_prompt = "custom"
allowed_tools = ["read_file"]
read_only = true
default_permission_mode = "ask"
"#,
        )
        .unwrap();
        let personas = resolve_personas(None, Some(tmp.path()));
        let plan = find_persona(&personas, "plan").unwrap();
        assert_eq!(plan.display_name, "Custom Plan");
        // Built-ins that weren't overridden are still present.
        assert!(find_persona(&personas, "code").is_some());
    }

    #[test]
    fn persona_infos_track_origin_and_crud_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path();

        // Pristine: all built-ins.
        let infos = resolve_persona_infos(Some(config), None);
        assert!(infos.iter().all(|i| i.origin == "builtin"));
        assert_eq!(infos[0].persona.id, "code");

        // Customize a built-in and add a custom persona.
        let mut plan = builtin_personas()
            .into_iter()
            .find(|m| m.id == "plan")
            .unwrap();
        plan.display_name = "My Plan".into();
        plan.default_model = Some("openai/gpt-4.1-mini".into());
        plan.default_thinking_level = Some("high".into());
        upsert_user_persona(config, &plan).unwrap();
        let custom = AgentPersona {
            id: "docs".into(),
            display_name: "Docs".into(),
            system_prompt: "write docs".into(),
            allowed_tools: vec![],
            read_only: false,
            default_permission_mode: None,
            default_model: None,
            default_thinking_level: None,
        };
        upsert_user_persona(config, &custom).unwrap();

        let infos = resolve_persona_infos(Some(config), None);
        let by_id = |id: &str| infos.iter().find(|i| i.persona.id == id).unwrap();
        assert_eq!(by_id("plan").origin, "customized");
        assert_eq!(by_id("plan").persona.display_name, "My Plan");
        assert_eq!(
            by_id("plan").persona.default_model.as_deref(),
            Some("openai/gpt-4.1-mini")
        );
        assert_eq!(
            by_id("plan").persona.default_thinking_level.as_deref(),
            Some("high")
        );
        assert_eq!(by_id("docs").origin, "custom");
        assert_eq!(by_id("code").origin, "builtin");
        // Built-ins keep canonical order; customs sort after.
        assert_eq!(infos.last().unwrap().persona.id, "docs");

        // Reset the built-in; remove the custom persona.
        delete_user_persona(config, "plan").unwrap();
        delete_user_persona(config, "docs").unwrap();
        let infos = resolve_persona_infos(Some(config), None);
        assert!(infos.iter().all(|i| i.origin == "builtin"));
        // Nothing left to delete.
        assert!(delete_user_persona(config, "plan").is_err());
        assert!(delete_user_persona(config, "docs").is_err());
    }

    #[test]
    fn invalid_persona_ids_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let mut persona = builtin_personas().remove(0);
        persona.id = "../evil".into();
        assert!(upsert_user_persona(tmp.path(), &persona).is_err());
        persona.id = "".into();
        assert!(upsert_user_persona(tmp.path(), &persona).is_err());
        assert!(is_valid_persona_id("review_2-alpha"));
        assert!(!is_valid_persona_id("../evil"));
        assert!(!is_valid_persona_id(""));
    }
}
