//! Agent skills: reusable instruction files discovered from the workspace
//! and the user's config dir.
//!
//! A skill is a `SKILL.md` with optional YAML-ish front matter. Skills are advertised in the
//! system prompt by stable name; the agent loads content through Trouve's
//! `load_skill` tool when relevant, so skill content never bloats the prompt
//! and host paths never become part of the model-facing contract.
//!
//! Discovery locations (later wins on name collision, workspace > global):
//!   1. Trouve's compiled-in provider-neutral skills
//!   2. `<config>/skills/*/SKILL.md`
//!   3. `<workspace>/.agents/skills/*/SKILL.md`

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Host-controlled path-list of additional resources that file-reading tools
/// may inspect without granting mutation access outside the session worktree.
pub const READ_ONLY_ROOTS_ENV: &str = "TROUVE_READ_ONLY_ROOTS";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// Directory name unless front matter overrides it.
    pub name: String,
    pub description: String,
    /// Whether the model may discover and load the skill implicitly.
    pub disable_model_invocation: bool,
    /// Whether the skill is shown as a slash command and may be explicitly
    /// invoked by a user.
    pub user_invocable: bool,
    /// Optional syntax shown after the command name.
    pub argument_hint: String,
    /// `builtin`, `user`, or `workspace`.
    pub origin: &'static str,
    source: SkillSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SkillSource {
    BuiltIn(&'static str),
    File(PathBuf),
}

#[derive(Debug, Default)]
struct FrontMatter {
    name: Option<String>,
    description: Option<String>,
    disable_model_invocation: bool,
    user_invocable: Option<bool>,
    argument_hint: Option<String>,
}

/// Parse `key: value` front matter between `---` fences at the top of a
/// SKILL.md.
fn parse_front_matter(text: &str) -> FrontMatter {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return FrontMatter::default();
    }
    let mut front = FrontMatter::default();
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            let value = value.trim().trim_matches(['"', '\'']).to_string();
            match key.trim() {
                "name" => front.name = Some(value),
                "description" => front.description = Some(value),
                "disable-model-invocation" => {
                    front.disable_model_invocation = value.eq_ignore_ascii_case("true")
                }
                "user-invocable" => {
                    front.user_invocable = if value.eq_ignore_ascii_case("true") {
                        Some(true)
                    } else if value.eq_ignore_ascii_case("false") {
                        Some(false)
                    } else {
                        None
                    }
                }
                "argument-hint" => front.argument_hint = Some(value),
                _ => {}
            }
        }
    }
    front
}

fn skill_from_text(
    directory: &str,
    text: &str,
    origin: &'static str,
    source: SkillSource,
) -> Option<Skill> {
    let front = parse_front_matter(text);
    let name = front.name.unwrap_or_else(|| directory.to_string());
    if !valid_skill_name(&name) {
        return None;
    }
    let description = front
        .description
        .unwrap_or_else(|| fallback_description(text));
    Some(Skill {
        name,
        description: description
            .chars()
            .take(MAX_SKILL_DESCRIPTION_CHARS)
            .collect(),
        disable_model_invocation: front.disable_model_invocation,
        user_invocable: front.user_invocable.unwrap_or(true),
        argument_hint: front.argument_hint.unwrap_or_default(),
        origin,
        source,
    })
}

fn fallback_description(text: &str) -> String {
    let mut lines = text.lines();
    let front_matter = lines.next().is_some_and(|line| line.trim() == "---");
    let body: Vec<_> = if front_matter {
        lines
            .skip_while(|line| line.trim() != "---")
            .skip(1)
            .collect()
    } else {
        text.lines().collect()
    };
    body.into_iter()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap_or("")
        .to_string()
}

fn load_dir(dir: &Path, origin: &'static str, out: &mut BTreeMap<String, Skill>) {
    let Ok(canonical_dir) = dir.canonicalize() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let skill_md = entry.path().join("SKILL.md");
        // Repositories can contain symlinks. A skill is allowed to read only
        // a SKILL.md physically contained by its declared skill root.
        let Ok(canonical_skill) = skill_md.canonicalize() else {
            continue;
        };
        if !canonical_skill.starts_with(&canonical_dir) {
            continue;
        }
        if std::fs::metadata(&canonical_skill).is_ok_and(|m| m.len() > MAX_SKILL_BYTES) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&canonical_skill) else {
            continue;
        };
        if text.len() as u64 > MAX_SKILL_BYTES {
            continue;
        }
        let directory = entry.file_name().to_string_lossy().to_string();
        if let Some(skill) = skill_from_text(
            &directory,
            &text,
            origin,
            SkillSource::File(canonical_skill),
        ) {
            out.insert(skill.name.clone(), skill);
        }
    }
}

/// Discover all skills visible to a thread. The built-in layer can be
/// disabled independently; user and workspace layers still resolve with
/// their normal precedence.
pub fn discover(
    config_dir: Option<&Path>,
    workspace_root: Option<&Path>,
    include_builtins: bool,
) -> Vec<Skill> {
    let mut skills = BTreeMap::new();
    if include_builtins {
        for builtin in BUILTIN_SKILLS {
            if let Some(skill) = skill_from_text(
                builtin.directory,
                builtin.text,
                "builtin",
                SkillSource::BuiltIn(builtin.text),
            ) {
                skills.insert(skill.name.clone(), skill);
            }
        }
    }
    if let Some(dir) = config_dir {
        load_dir(&dir.join("skills"), "user", &mut skills);
    }
    if let Some(root) = workspace_root {
        load_dir(
            &root.join(".agents").join("skills"),
            "workspace",
            &mut skills,
        );
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
    let advertised: Vec<_> = skills
        .iter()
        .filter(|skill| !skill.disable_model_invocation)
        .collect();
    if advertised.is_empty() {
        return None;
    }
    let mut section = String::from(
        "## Available skills\n\nWhen a task matches a skill below, call `load_skill` with its \
         name and follow the returned instructions before proceeding.\n",
    );
    for skill in advertised {
        section.push_str(&format!("\n- **{}** — {}", skill.name, skill.description));
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

        let skills = discover(Some(&cfg), Some(&repo), true);
        assert_eq!(skills.len(), BUILTIN_SKILLS.len() + 2);
        let deploy = skills.iter().find(|s| s.name == "deploy").unwrap();
        assert_eq!(deploy.description, "Repo deploy skill");
        assert_eq!(deploy.origin, "workspace");
        let workspace_skill_root = repo.join(".agents").canonicalize().unwrap();
        assert!(matches!(
            &deploy.source,
            SkillSource::File(path) if path.starts_with(&workspace_skill_root)
        ));
        let review = skills.iter().find(|s| s.name == "review").unwrap();
        assert_eq!(review.description, "How to review PRs here.");
    }

    #[test]
    fn prompt_section_lists_skills() {
        let skills = vec![Skill {
            name: "write-adr".into(),
            description: "Write an ADR".into(),
            disable_model_invocation: false,
            user_invocable: true,
            argument_hint: String::new(),
            origin: "workspace",
            source: SkillSource::File("/x/SKILL.md".into()),
        }];
        let section = prompt_section(&skills).unwrap();
        assert!(section.contains("write-adr"));
        assert!(section.contains("load_skill"));
        assert!(!section.contains("/x/SKILL.md"));
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
