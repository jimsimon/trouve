//! Data-driven agent modes (invariant 6): a mode is a prompt, a tool
//! policy, and a default permission mode. Built-ins ship as data; users add
//! or override modes with TOML files in `<config>/modes/` or a workspace's
//! `.agents/modes/`.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use trouve_protocol::{AgentMode, ModeInfo, PermissionMode};

pub fn builtin_modes() -> Vec<AgentMode> {
    vec![
        AgentMode {
            id: "code".into(),
            display_name: "Code".into(),
            system_prompt: "You are in code mode: implement the user's request by editing \
                            files in the workspace. Prefer small verifiable steps; run tests \
                            or builds when they exist. Report what you changed when done."
                .into(),
            allowed_tools: vec![],
            read_only: false,
            default_permission_mode: PermissionMode::Ask,
            default_model: None,
        },
        AgentMode {
            id: "plan".into(),
            display_name: "Plan".into(),
            system_prompt: "You are in plan mode: explore the workspace and produce a concrete \
                            implementation plan. Do not modify any files; your deliverable is \
                            the plan itself."
                .into(),
            allowed_tools: vec![
                "read_file".into(),
                "list_dir".into(),
                "grep".into(),
                "search".into(),
                "find_related".into(),
            ],
            read_only: true,
            default_permission_mode: PermissionMode::Ask,
            default_model: None,
        },
        AgentMode {
            id: "review".into(),
            display_name: "Review".into(),
            system_prompt: "You are in review mode: examine the changes in this workspace and \
                            report problems — bugs, missed edge cases, style violations — with \
                            file and line references. Do not modify files."
                .into(),
            allowed_tools: vec![
                "read_file".into(),
                "list_dir".into(),
                "grep".into(),
                "search".into(),
                "find_related".into(),
                "shell".into(),
            ],
            read_only: true,
            default_permission_mode: PermissionMode::Ask,
            default_model: None,
        },
        AgentMode {
            id: "architect".into(),
            display_name: "Architect".into(),
            system_prompt: "You are in architect mode: reason about structure, boundaries, and \
                            trade-offs. Propose designs and ADR-style records rather than \
                            direct code changes; only touch documentation files."
                .into(),
            allowed_tools: vec![],
            read_only: false,
            default_permission_mode: PermissionMode::Ask,
            default_model: None,
        },
        AgentMode {
            id: "question".into(),
            display_name: "Question".into(),
            system_prompt: "You are in question mode: answer questions about the workspace. \
                            Read whatever you need; never modify anything."
                .into(),
            allowed_tools: vec![
                "read_file".into(),
                "list_dir".into(),
                "grep".into(),
                "search".into(),
                "find_related".into(),
            ],
            read_only: true,
            default_permission_mode: PermissionMode::Ask,
            default_model: None,
        },
    ]
}

fn load_dir(dir: &Path, modes: &mut Vec<AgentMode>) {
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
        match toml::from_str::<AgentMode>(&text) {
            Ok(mode) => {
                // Later layers override earlier ones by id.
                modes.retain(|m| m.id != mode.id);
                modes.push(mode);
            }
            Err(e) => tracing::warn!("ignoring invalid mode file {}: {e}", path.display()),
        }
    }
}

/// Built-ins, overlaid by `<config>/modes/*.toml`, overlaid by the
/// workspace's `.agents/modes/*.toml`.
pub fn resolve_modes(config_dir: Option<&Path>, workspace_root: Option<&Path>) -> Vec<AgentMode> {
    let mut modes = builtin_modes();
    if let Some(dir) = config_dir {
        load_dir(&dir.join("modes"), &mut modes);
    }
    if let Some(root) = workspace_root {
        load_dir(&root.join(".agents").join("modes"), &mut modes);
    }
    modes
}

pub fn find_mode<'a>(modes: &'a [AgentMode], id: &str) -> Option<&'a AgentMode> {
    modes.iter().find(|m| m.id == id)
}

/// Modes with provenance for the settings UI. Same layering as
/// [`resolve_modes`]; each entry is tagged with where its effective
/// definition came from.
pub fn resolve_mode_infos(
    config_dir: Option<&Path>,
    workspace_root: Option<&Path>,
) -> Vec<ModeInfo> {
    let builtin_ids: Vec<String> = builtin_modes().iter().map(|m| m.id.clone()).collect();
    let mut infos: Vec<ModeInfo> = builtin_modes()
        .into_iter()
        .map(|mode| ModeInfo {
            mode,
            origin: "builtin".into(),
        })
        .collect();
    let mut overlay = |dir: &Path, origin_over_builtin: &str, origin_new: &str| {
        let mut modes = Vec::new();
        load_dir(dir, &mut modes);
        for mode in modes {
            let origin = if builtin_ids.contains(&mode.id) {
                origin_over_builtin.to_string()
            } else {
                origin_new.to_string()
            };
            infos.retain(|i| i.mode.id != mode.id);
            infos.push(ModeInfo { mode, origin });
        }
    };
    if let Some(dir) = config_dir {
        overlay(&dir.join("modes"), "customized", "custom");
    }
    if let Some(root) = workspace_root {
        let dir = root.join(".agents").join("modes");
        overlay(&dir, "workspace", "workspace");
    }
    // Stable order: built-ins first in their canonical order, then the rest
    // alphabetically.
    infos.sort_by_key(|i| {
        (
            builtin_ids
                .iter()
                .position(|id| *id == i.mode.id)
                .unwrap_or(usize::MAX),
            i.mode.id.clone(),
        )
    });
    infos
}

/// The user-level mode file defining `id`, if any. Prefers `<id>.toml` but
/// falls back to scanning (files may be named freely).
fn user_mode_file(config_dir: &Path, id: &str) -> Option<PathBuf> {
    let dir = config_dir.join("modes");
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
        if let Ok(mode) = toml::from_str::<AgentMode>(&text) {
            if mode.id == id {
                return Some(path);
            }
        }
    }
    None
}

/// Write (create or replace) the user-level TOML file for a mode. Saving
/// under a built-in id customizes that built-in.
pub fn upsert_user_mode(config_dir: &Path, mode: &AgentMode) -> Result<()> {
    if mode.id.is_empty()
        || !mode
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("mode id must be non-empty and [a-zA-Z0-9_-] only");
    }
    let path = user_mode_file(config_dir, &mode.id)
        .unwrap_or_else(|| config_dir.join("modes").join(format!("{}.toml", mode.id)));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let text = toml::to_string_pretty(mode).context("serializing mode")?;
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Remove the user-level file for a mode: deletes a custom mode outright,
/// or resets a customized built-in back to its defaults.
pub fn delete_user_mode(config_dir: &Path, id: &str) -> Result<()> {
    let Some(path) = user_mode_file(config_dir, id) else {
        if builtin_modes().iter().any(|m| m.id == id) {
            bail!("mode '{id}' is a built-in with no user override to remove");
        }
        bail!("no user-level mode '{id}'");
    };
    std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_mode_overrides_builtin() {
        let tmp = tempfile::tempdir().unwrap();
        let modes_dir = tmp.path().join(".agents/modes");
        std::fs::create_dir_all(&modes_dir).unwrap();
        std::fs::write(
            modes_dir.join("plan.toml"),
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
        let modes = resolve_modes(None, Some(tmp.path()));
        let plan = find_mode(&modes, "plan").unwrap();
        assert_eq!(plan.display_name, "Custom Plan");
        // Built-ins that weren't overridden are still present.
        assert!(find_mode(&modes, "code").is_some());
    }

    #[test]
    fn mode_infos_track_origin_and_crud_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path();

        // Pristine: all built-ins.
        let infos = resolve_mode_infos(Some(config), None);
        assert!(infos.iter().all(|i| i.origin == "builtin"));
        assert_eq!(infos[0].mode.id, "code");

        // Customize a built-in and add a custom mode.
        let mut plan = builtin_modes()
            .into_iter()
            .find(|m| m.id == "plan")
            .unwrap();
        plan.display_name = "My Plan".into();
        plan.default_model = Some("openai/gpt-4.1-mini".into());
        upsert_user_mode(config, &plan).unwrap();
        let custom = AgentMode {
            id: "docs".into(),
            display_name: "Docs".into(),
            system_prompt: "write docs".into(),
            allowed_tools: vec![],
            read_only: false,
            default_permission_mode: PermissionMode::Ask,
            default_model: None,
        };
        upsert_user_mode(config, &custom).unwrap();

        let infos = resolve_mode_infos(Some(config), None);
        let by_id = |id: &str| infos.iter().find(|i| i.mode.id == id).unwrap();
        assert_eq!(by_id("plan").origin, "customized");
        assert_eq!(by_id("plan").mode.display_name, "My Plan");
        assert_eq!(
            by_id("plan").mode.default_model.as_deref(),
            Some("openai/gpt-4.1-mini")
        );
        assert_eq!(by_id("docs").origin, "custom");
        assert_eq!(by_id("code").origin, "builtin");
        // Built-ins keep canonical order; customs sort after.
        assert_eq!(infos.last().unwrap().mode.id, "docs");

        // Reset the built-in; remove the custom mode.
        delete_user_mode(config, "plan").unwrap();
        delete_user_mode(config, "docs").unwrap();
        let infos = resolve_mode_infos(Some(config), None);
        assert!(infos.iter().all(|i| i.origin == "builtin"));
        // Nothing left to delete.
        assert!(delete_user_mode(config, "plan").is_err());
        assert!(delete_user_mode(config, "docs").is_err());
    }

    #[test]
    fn invalid_mode_ids_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mode = builtin_modes().remove(0);
        mode.id = "../evil".into();
        assert!(upsert_user_mode(tmp.path(), &mode).is_err());
        mode.id = "".into();
        assert!(upsert_user_mode(tmp.path(), &mode).is_err());
    }
}
