//! Data-driven agent modes (invariant 6): a mode is a prompt, a tool
//! policy, and a default permission mode. Built-ins ship as data; users add
//! or override modes with TOML files in `<config>/modes/` or a workspace's
//! `.agents/modes/`.

use std::path::Path;

use trouve_protocol::{AgentMode, PermissionMode};

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
}
