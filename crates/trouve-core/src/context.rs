//! System prompt assembly with layered AGENTS.md discovery.
//!
//! Layers, in order (later layers appear later in the prompt so they read
//! as more specific guidance):
//!   1. trouve's base prompt
//!   2. the agent mode's prompt
//!   3. global user instructions   (`<config>/AGENTS.md`)
//!   4. workspace instructions     (`<repo>/AGENTS.md`, `<repo>/.agents/AGENTS.md`)

use std::path::Path;

use trouve_protocol::AgentMode;

const BASE_PROMPT: &str = "You are trouve, an AI coding agent operating inside a dedicated git \
worktree for this session. You interact with the workspace exclusively through the provided \
tools. Tool calls may require user approval; if a call is denied, respect the decision and \
adapt. Be precise, verify your changes, and keep the user informed of what you are doing.";

fn read_if_exists(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Assemble the full system prompt for a thread.
pub fn system_prompt(mode: &AgentMode, config_dir: Option<&Path>, workspace_root: &Path) -> String {
    let mut sections = vec![BASE_PROMPT.to_string(), mode.system_prompt.clone()];

    if let Some(dir) = config_dir {
        if let Some(text) = read_if_exists(&dir.join("AGENTS.md")) {
            sections.push(format!("## User instructions (global AGENTS.md)\n\n{text}"));
        }
    }
    for candidate in [
        workspace_root.join("AGENTS.md"),
        workspace_root.join(".agents").join("AGENTS.md"),
    ] {
        if let Some(text) = read_if_exists(&candidate) {
            sections.push(format!(
                "## Workspace instructions ({})\n\n{text}",
                candidate
                    .strip_prefix(workspace_root)
                    .unwrap_or(&candidate)
                    .display()
            ));
        }
    }
    let skills = crate::skills::discover(config_dir, Some(workspace_root));
    if let Some(section) = crate::skills::prompt_section(&skills) {
        sections.push(section);
    }
    sections.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::builtin_modes;

    #[test]
    fn layers_are_ordered_and_optional() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("cfg");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::create_dir_all(repo.join(".agents")).unwrap();
        std::fs::write(cfg.join("AGENTS.md"), "GLOBAL RULES").unwrap();
        std::fs::write(repo.join("AGENTS.md"), "REPO RULES").unwrap();
        std::fs::write(repo.join(".agents/AGENTS.md"), "DOTAGENTS RULES").unwrap();

        let modes = builtin_modes();
        let prompt = system_prompt(&modes[0], Some(&cfg), &repo);
        let g = prompt.find("GLOBAL RULES").unwrap();
        let r = prompt.find("REPO RULES").unwrap();
        let d = prompt.find("DOTAGENTS RULES").unwrap();
        assert!(g < r && r < d, "layer order: global < repo < .agents");

        // No files at all: still a usable prompt.
        let empty = tmp.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let prompt = system_prompt(&modes[0], None, &empty);
        assert!(prompt.contains("You are trouve"));
    }
}
