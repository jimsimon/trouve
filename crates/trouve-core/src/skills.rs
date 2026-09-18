//! Agent skills: reusable instruction files discovered from the workspace
//! and the user's config dir.
//!
//! A skill is a directory containing `SKILL.md` with optional YAML-ish
//! front matter (`name:`, `description:`). Skills are advertised in the
//! system prompt with their path; the agent reads the file with its normal
//! tools when a skill is relevant, so skill content never bloats the prompt.
//!
//! Discovery locations (later wins on name collision, workspace > global):
//!   1. `<config>/skills/*/SKILL.md`
//!   2. `<workspace>/.agents/skills/*/SKILL.md`
//!
//! The engine owns this roster for every route. It is published to clients
//! as slash-command completions, advertised to native and vendor models
//! alike, and a prompt that starts with `/<skill>` is expanded here before
//! it reaches any model. Vendor-native skill mechanisms are switched off in
//! the adapters so that this directory layout is the only one that matters.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Host-controlled path-list of additional resources that file-reading tools
/// may inspect without granting mutation access outside the session worktree.
pub const READ_ONLY_ROOTS_ENV: &str = "TROUVE_READ_ONLY_ROOTS";

/// Upper bound on how much of a SKILL.md is inlined for a `/skill`
/// invocation. Larger files are truncated with a pointer back to the path so
/// the model can read the remainder with its file tools.
const MAX_INLINED_SKILL_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// Directory name unless front matter overrides it.
    pub name: String,
    pub description: String,
    /// Absolute path to the SKILL.md file.
    pub path: PathBuf,
}

impl Skill {
    /// Composer completion entry for this skill.
    pub fn command_info(&self) -> trouve_protocol::CommandInfo {
        trouve_protocol::CommandInfo {
            name: self.name.clone(),
            description: self.description.clone(),
        }
    }
}

/// A prompt whose first token is `/<skill>` for a discovered skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation<'s, 'p> {
    pub skill: &'s Skill,
    /// Everything after the command token, trimmed.
    pub arguments: &'p str,
}

/// Split a leading `/name` off `prompt`. Only the first token counts, so
/// paths and URLs later in the message are never mistaken for commands.
fn parse_slash(prompt: &str) -> Option<(&str, &str)> {
    let trimmed = prompt.trim_start();
    let body = trimmed.strip_prefix('/')?;
    let end = body.find(char::is_whitespace).unwrap_or(body.len());
    let name = &body[..end];
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'));
    valid.then(|| (name, body[end..].trim()))
}

/// Resolve a `/skill` invocation at the start of `prompt` against the
/// discovered roster. Unknown names are left alone; they are ordinary text.
pub fn invocation<'s, 'p>(prompt: &'p str, skills: &'s [Skill]) -> Option<Invocation<'s, 'p>> {
    let (name, arguments) = parse_slash(prompt)?;
    let skill = skills.iter().find(|skill| skill.name == name)?;
    Some(Invocation { skill, arguments })
}

/// SKILL.md body with the front matter fence removed.
fn strip_front_matter(text: &str) -> &str {
    let mut offset = 0;
    for (index, segment) in text.split_inclusive('\n').enumerate() {
        offset += segment.len();
        if index == 0 {
            if segment.trim() != "---" {
                return text;
            }
            continue;
        }
        if segment.trim() == "---" {
            return &text[offset..];
        }
    }
    text
}

/// Render the instruction block for an invoked skill. The same block goes to
/// every route: vendor backends receive it inline in the prompt, native
/// providers in the system prompt, so the transcript keeps the user's
/// original `/skill` text.
pub fn invocation_block(invocation: &Invocation<'_, '_>) -> String {
    let skill = invocation.skill;
    let raw = std::fs::read_to_string(&skill.path).unwrap_or_default();
    let mut body = strip_front_matter(&raw).trim().to_string();
    if body.len() > MAX_INLINED_SKILL_BYTES {
        let mut cut = MAX_INLINED_SKILL_BYTES;
        while !body.is_char_boundary(cut) {
            cut -= 1;
        }
        body.truncate(cut);
        body.push_str(&format!(
            "\n\n[truncated; read {} for the rest]",
            skill.path.display()
        ));
    }
    let mut block = format!(
        "The user invoked the `{name}` skill with `/{name}`. Follow the skill \
         instructions below for this request.\n\n\
         <invoked_skill name=\"{name}\" path=\"{path}\">\n{body}\n</invoked_skill>",
        name = skill.name,
        path = skill.path.display(),
    );
    if !invocation.arguments.is_empty() {
        block.push_str(&format!(
            "\n\nArguments supplied with the invocation: {}",
            invocation.arguments
        ));
    }
    block
}

/// Prompt text for a vendor backend: a leading `/skill` is replaced by the
/// skill block followed by the remaining text, so the vendor never sees a
/// bare slash command it might route to its own command handling.
pub fn expand_invocation(prompt: &str, skills: &[Skill]) -> String {
    match invocation(prompt, skills) {
        Some(invocation) => {
            let mut expanded = invocation_block(&invocation);
            if !invocation.arguments.is_empty() {
                expanded.push_str("\n\n");
                expanded.push_str(invocation.arguments);
            }
            expanded
        }
        None => prompt.to_string(),
    }
}

/// Parse `key: value` front matter between `---` fences at the top of a
/// SKILL.md. Returns (name, description) if present.
fn parse_front_matter(text: &str) -> (Option<String>, Option<String>) {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, None);
    }
    let mut name = None;
    let mut description = None;
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            let value = value.trim().trim_matches('"').to_string();
            match key.trim() {
                "name" => name = Some(value),
                "description" => description = Some(value),
                _ => {}
            }
        }
    }
    (name, description)
}

fn load_dir(dir: &Path, out: &mut BTreeMap<String, Skill>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let skill_md = entry.path().join("SKILL.md");
        let Ok(text) = std::fs::read_to_string(&skill_md) else {
            continue;
        };
        let dir_name = entry.file_name().to_string_lossy().to_string();
        let (name, description) = parse_front_matter(&text);
        let name = name.unwrap_or(dir_name);
        let description = description.unwrap_or_else(|| {
            // Fall back to the first non-heading, non-empty line.
            text.lines()
                .map(str::trim)
                .find(|l| !l.is_empty() && !l.starts_with('#') && *l != "---")
                .unwrap_or("")
                .to_string()
        });
        out.insert(
            name.clone(),
            Skill {
                name,
                description,
                path: skill_md,
            },
        );
    }
}

/// Discover all skills visible to a thread.
pub fn discover(config_dir: Option<&Path>, workspace_root: Option<&Path>) -> Vec<Skill> {
    let mut skills = BTreeMap::new();
    if let Some(dir) = config_dir {
        load_dir(&dir.join("skills"), &mut skills);
    }
    if let Some(root) = workspace_root {
        load_dir(&root.join(".agents").join("skills"), &mut skills);
    }
    skills.into_values().collect()
}

fn configured_home(variable: &str, fallback: &Path) -> PathBuf {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| fallback.to_path_buf())
}

fn canonical_existing_roots(candidates: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut roots = candidates
        .into_iter()
        .filter_map(|candidate| candidate.canonicalize().ok())
        // A root filesystem is never a reasonable resource capability. This
        // also turns an accidentally empty path-list entry into a fail-closed
        // no-op instead of broad host access.
        .filter(|root| root.parent().is_some())
        .filter(|root| {
            std::fs::metadata(root).is_ok_and(|metadata| metadata.is_dir() || metadata.is_file())
        })
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    roots
}

/// Canonical host resources visible to read-only filesystem tools.
///
/// These are deliberately narrower than the user's home/config directories:
/// only directories whose contents are intended to be agent instructions or
/// installed plugin packages are automatic. Embedders may add explicit roots
/// through [`READ_ONLY_ROOTS_ENV`]. Missing roots are ignored, and callers
/// still validate every requested path after resolving symlinks.
pub fn trusted_read_roots(
    config_dir: Option<&Path>,
    workspace_root: Option<&Path>,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(config_dir) = config_dir {
        candidates.push(config_dir.join("skills"));
    }
    if let Some(workspace_root) = workspace_root {
        candidates.push(workspace_root.join(".agents").join("skills"));
    }

    if let Some(home) = dirs::home_dir() {
        let codex = configured_home("CODEX_HOME", &home.join(".codex"));
        candidates.push(codex.join("skills"));
        candidates.push(codex.join("plugins").join("cache"));

        let claude = configured_home("CLAUDE_CONFIG_DIR", &home.join(".claude"));
        candidates.push(claude.join("skills"));
        candidates.push(claude.join("plugins").join("cache"));

        candidates.push(home.join(".cursor").join("skills"));
    }

    if let Some(raw) = std::env::var_os(READ_ONLY_ROOTS_ENV) {
        candidates.extend(std::env::split_paths(&raw).filter(|path| path.is_absolute()));
    }
    canonical_existing_roots(candidates)
}

/// Render the "available skills" section of the system prompt, or None when
/// there are no skills.
pub fn prompt_section(skills: &[Skill]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }
    let mut section = String::from(
        "## Available skills\n\nWhen a task matches a skill below, read its SKILL.md with your \
         file-reading tool and follow it before proceeding. The user can also invoke a skill \
         explicitly by starting a message with `/<skill name>`.\n",
    );
    for skill in skills {
        section.push_str(&format!(
            "\n- **{}** — {} ({})",
            skill.name,
            skill.description,
            skill.path.display()
        ));
    }
    Some(section)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(root: &Path, dir: &str, contents: &str) {
        let d = root.join(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("SKILL.md"), contents).unwrap();
    }

    #[test]
    fn slash_invocation_matches_only_the_leading_token() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        write_skill(
            &repo,
            ".agents/skills/ship",
            "---\nname: ship\ndescription: Ship it\n---\n# Ship\n\nRun the release checklist.\n",
        );
        let skills = discover(None, Some(&repo));

        let hit = invocation("  /ship to staging", &skills).expect("skill invoked");
        assert_eq!(hit.skill.name, "ship");
        assert_eq!(hit.arguments, "to staging");
        assert!(invocation("/unknown thing", &skills).is_none());
        assert!(invocation("see /ship later", &skills).is_none());
        assert!(invocation("/ship/SKILL.md", &skills).is_none());
        assert_eq!(invocation("/ship", &skills).unwrap().arguments, "");

        let block = invocation_block(&hit);
        assert!(block.contains("<invoked_skill name=\"ship\""));
        assert!(block.contains("Run the release checklist."));
        assert!(
            !block.contains("description: Ship it"),
            "front matter is stripped"
        );
        assert!(block.contains("Arguments supplied with the invocation: to staging"));

        let expanded = expand_invocation("/ship to staging", &skills);
        assert!(expanded.starts_with("The user invoked the `ship` skill"));
        assert!(expanded.ends_with("\n\nto staging"));
        assert_eq!(expand_invocation("plain text", &skills), "plain text");
        assert_eq!(expand_invocation("/unknown", &skills), "/unknown");
    }

    #[test]
    fn oversized_skill_bodies_are_truncated_with_a_pointer() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let body = "x".repeat(MAX_INLINED_SKILL_BYTES + 100);
        write_skill(&repo, ".agents/skills/big", &format!("# Big\n\n{body}"));
        let skills = discover(None, Some(&repo));
        let block = invocation_block(&invocation("/big", &skills).unwrap());
        assert!(block.contains("[truncated; read "));
        assert!(block.len() < MAX_INLINED_SKILL_BYTES + 1024);
    }

    #[test]
    fn discovers_and_merges_with_workspace_priority() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("cfg");
        let repo = tmp.path().join("repo");
        write_skill(
            &cfg,
            "skills/deploy",
            "---\nname: deploy\ndescription: Global deploy skill\n---\nsteps",
        );
        write_skill(
            &repo,
            ".agents/skills/deploy",
            "---\nname: deploy\ndescription: Repo deploy skill\n---\nsteps",
        );
        write_skill(
            &repo,
            ".agents/skills/review",
            "# Review\n\nHow to review PRs here.",
        );

        let skills = discover(Some(&cfg), Some(&repo));
        assert_eq!(skills.len(), 2);
        let deploy = skills.iter().find(|s| s.name == "deploy").unwrap();
        assert_eq!(deploy.description, "Repo deploy skill");
        assert!(deploy.path.starts_with(repo.join(".agents")));
        let review = skills.iter().find(|s| s.name == "review").unwrap();
        assert_eq!(review.description, "How to review PRs here.");
    }

    #[test]
    fn prompt_section_lists_skills() {
        let skills = vec![Skill {
            name: "write-adr".into(),
            description: "Write an ADR".into(),
            path: "/x/SKILL.md".into(),
        }];
        let section = prompt_section(&skills).unwrap();
        assert!(section.contains("write-adr"));
        assert!(section.contains("/x/SKILL.md"));
        assert!(prompt_section(&[]).is_none());
    }

    #[test]
    fn canonical_read_roots_ignore_missing_duplicates_and_filesystem_root() {
        let tmp = tempfile::tempdir().unwrap();
        let resource = tmp.path().join("resource");
        std::fs::create_dir(&resource).unwrap();
        let alias = tmp.path().join("alias");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&resource, &alias).unwrap();

        let mut candidates = vec![
            resource.clone(),
            resource.clone(),
            tmp.path().join("missing"),
        ];
        #[cfg(unix)]
        candidates.extend([alias, PathBuf::from("/")]);

        assert_eq!(
            canonical_existing_roots(candidates),
            vec![resource.canonicalize().unwrap()]
        );
    }
}
