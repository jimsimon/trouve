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
use std::io::Read;
use std::path::{Path, PathBuf};

/// Host-controlled path-list of additional resources that file-reading tools
/// may inspect without granting mutation access outside the session worktree.
pub const READ_ONLY_ROOTS_ENV: &str = "TROUVE_READ_ONLY_ROOTS";

/// Upper bound on how much of a SKILL.md is inlined for a `/skill`
/// invocation. Larger files are truncated with a pointer back to the path so
/// the model can read the remainder with its file tools.
const MAX_INLINED_SKILL_BYTES: usize = 32 * 1024;
/// Bytes actually read from a SKILL.md: the inlined body plus headroom for
/// front matter. Nothing past this offset is ever loaded into memory.
const MAX_SKILL_READ_BYTES: usize = MAX_INLINED_SKILL_BYTES + 4 * 1024;
/// Roster descriptions are workspace-controlled; keep each one short so the
/// catalog, the published command list, and the composer stay bounded.
const MAX_DESCRIPTION_CHARS: usize = 200;
/// Byte budget for the advertised catalog in a system prompt or vendor
/// instructions. Entries past the budget are summarised as a count.
const MAX_CATALOG_BYTES: usize = 12 * 1024;
/// Deterministic cap on skill directories examined per root (sorted by
/// name). Bounds discovery I/O, the persisted `CommandsUpdated` payload,
/// and client state for a workspace with an enormous skills tree.
const MAX_SKILLS_PER_ROOT: usize = 256;
/// Longest publishable command name; longer names are not typed back.
const MAX_COMMAND_NAME_CHARS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// Directory name unless front matter overrides it.
    pub name: String,
    pub description: String,
    /// Absolute path to the SKILL.md file.
    pub path: PathBuf,
    /// Canonical base (config dir or workspace root) and the link-free
    /// relative path beneath it. Every read walks `relative` component by
    /// component from `base` without following symlinks, so a link swapped
    /// in after discovery cannot pull foreign content into a prompt.
    base: PathBuf,
    relative: PathBuf,
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

/// One grammar for publication and parsing: any non-empty token without
/// whitespace can be published as `/<name>` and typed back. Roster matching
/// is the only other validity check.
fn is_command_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= MAX_COMMAND_NAME_CHARS
        && !name.starts_with('/')
        && !name.contains(char::is_whitespace)
}

/// Split a leading `/name` off `prompt`. Only the first token counts, so
/// paths and URLs later in the message are never mistaken for commands.
fn parse_slash(prompt: &str) -> Option<(&str, &str)> {
    let trimmed = prompt.trim_start();
    let body = trimmed.strip_prefix('/')?;
    let end = body.find(char::is_whitespace).unwrap_or(body.len());
    let name = &body[..end];
    is_command_name(name).then(|| (name, body[end..].trim()))
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

/// Open `base/relative` without following any symlink: the base directory
/// is opened by its canonical path, then every component is opened relative
/// to the previous directory handle with `O_NOFOLLOW`. Because the walk uses
/// handles rather than re-resolving a checked pathname, a concurrent writer
/// swapping an ancestor for a symlink cannot redirect the open.
#[cfg(unix)]
fn open_beneath(base: &Path, relative: &Path) -> Option<std::fs::File> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    let components: Vec<&std::ffi::OsStr> = relative
        .components()
        .map(|component| match component {
            std::path::Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect::<Option<_>>()?;
    let (leaf, directories) = components.split_last()?;
    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW);
    let mut parent: OwnedFd = options.open(base).ok()?.into();
    let open_at = |parent: &OwnedFd, name: &std::ffi::OsStr, flags: libc::c_int| {
        let name = CString::new(name.as_bytes()).ok()?;
        // SAFETY: `parent` is an open descriptor and `name` a valid C string
        // for the duration of the call; the returned fd is owned immediately.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | flags,
            )
        };
        (fd >= 0).then(|| {
            // SAFETY: openat returned one newly-owned descriptor.
            unsafe { OwnedFd::from_raw_fd(fd) }
        })
    };
    for directory in directories {
        parent = open_at(&parent, directory, libc::O_DIRECTORY)?;
    }
    let file: std::fs::File = open_at(&parent, leaf, 0)?.into();
    // A FIFO or device would block or stream; only regular files are skills.
    file.metadata().ok()?.is_file().then_some(file)
}

/// Portable fallback: verify the canonical target stays beneath `base`, then
/// open. Not race-free, but symlink escapes at rest are still refused.
#[cfg(not(unix))]
fn open_beneath(base: &Path, relative: &Path) -> Option<std::fs::File> {
    let canonical = base.join(relative).canonicalize().ok()?;
    if !canonical.starts_with(base) {
        return None;
    }
    let file = std::fs::File::open(&canonical).ok()?;
    file.metadata().ok()?.is_file().then_some(file)
}

/// Bounded, confined read of a SKILL.md beneath `base`. At most
/// [`MAX_SKILL_READ_BYTES`] are read. Returns the text and whether the file
/// continued past the cap.
fn read_confined(base: &Path, relative: &Path) -> Option<(String, bool)> {
    let file = open_beneath(base, relative)?;
    let mut bytes = Vec::new();
    file.take(MAX_SKILL_READ_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    let truncated = bytes.len() > MAX_SKILL_READ_BYTES;
    bytes.truncate(MAX_SKILL_READ_BYTES);
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => {
            // A cap can land inside a multi-byte character; drop the partial
            // tail rather than the whole file. Invalid UTF-8 earlier in the
            // file is treated the same way so nothing is guessed at.
            let valid = error.utf8_error().valid_up_to();
            let mut bytes = error.into_bytes();
            bytes.truncate(valid);
            String::from_utf8(bytes).unwrap_or_default()
        }
    };
    Some((text, truncated))
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut out: String = text.chars().take(max_chars).collect();
    if out.len() < text.len() {
        out.push('…');
    }
    out
}

/// Render the instruction block for an invoked skill. The same block goes to
/// every route: vendor backends receive it inline in the prompt, native
/// providers in the system prompt, so the transcript keeps the user's
/// original `/skill` text.
pub fn invocation_block(invocation: &Invocation<'_, '_>) -> String {
    let skill = invocation.skill;
    let Some((raw, read_truncated)) = read_confined(&skill.base, &skill.relative) else {
        return format!(
            "The user invoked `/{name}`, but its SKILL.md at {path} could not be read \
             from within its skills directory (missing, unreadable, or reached through \
             a symlink). Tell the user the skill is unavailable and do not guess at \
             its contents.",
            name = skill.name,
            path = skill.path.display(),
        );
    };
    let mut body = strip_front_matter(&raw).trim();
    let mut body_truncated = read_truncated;
    if body.len() > MAX_INLINED_SKILL_BYTES {
        let mut cut = MAX_INLINED_SKILL_BYTES;
        while !body.is_char_boundary(cut) {
            cut -= 1;
        }
        body = &body[..cut];
        body_truncated = true;
    }
    let truncation_note = if body_truncated {
        format!(
            "\n\n[truncated; read {} for the rest]",
            skill.path.display()
        )
    } else {
        String::new()
    };
    let mut block = format!(
        "The user invoked the `{name}` skill with `/{name}`. Follow the skill \
         instructions below for this request.\n\n\
         <invoked_skill name=\"{name}\" path=\"{path}\">\n{body}{truncation_note}\n</invoked_skill>",
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

/// Load skills from `base/<skills_dir>/*/SKILL.md`. Directory listing is
/// advisory (it only decides which names are tried); every read walks from
/// the canonical `base` without following symlinks.
fn load_dir(base: &Path, skills_dir: &Path, out: &mut BTreeMap<String, Skill>) {
    let Ok(base) = base.canonicalize() else {
        return;
    };
    let dir = base.join(skills_dir);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    // Deterministic order and a hard cap: the same tree always yields the
    // same roster, and a huge tree cannot make discovery unbounded.
    let mut names: Vec<std::ffi::OsString> =
        entries.flatten().map(|entry| entry.file_name()).collect();
    names.sort();
    for dir_name in names.into_iter().take(MAX_SKILLS_PER_ROOT) {
        let relative = skills_dir.join(&dir_name).join("SKILL.md");
        let Some((text, _)) = read_confined(&base, &relative) else {
            continue;
        };
        let dir_name = dir_name.to_string_lossy().to_string();
        let (name, description) = parse_front_matter(&text);
        let name = name.unwrap_or(dir_name);
        if !is_command_name(&name) {
            continue;
        }
        let description = description.unwrap_or_else(|| {
            // Fall back to the first non-heading, non-empty line.
            text.lines()
                .map(str::trim)
                .find(|l| !l.is_empty() && !l.starts_with('#') && *l != "---")
                .unwrap_or("")
                .to_string()
        });
        let description = truncate_chars(&description, MAX_DESCRIPTION_CHARS);
        out.insert(
            name.clone(),
            Skill {
                name,
                description,
                path: base.join(&relative),
                base: base.clone(),
                relative,
            },
        );
    }
}

/// Discover all skills visible to a thread.
pub fn discover(config_dir: Option<&Path>, workspace_root: Option<&Path>) -> Vec<Skill> {
    let mut skills = BTreeMap::new();
    if let Some(dir) = config_dir {
        load_dir(dir, Path::new("skills"), &mut skills);
    }
    if let Some(root) = workspace_root {
        load_dir(root, Path::new(".agents/skills"), &mut skills);
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
    for (index, skill) in skills.iter().enumerate() {
        let entry = format!(
            "\n- **{}** — {} ({})",
            skill.name,
            skill.description,
            skill.path.display()
        );
        // Deterministic budget: the roster is workspace-controlled, so an
        // oversized one must not consume the model's context or exceed a
        // vendor's request limit. Skills stay invocable by name regardless.
        if section.len() + entry.len() > MAX_CATALOG_BYTES {
            section.push_str(&format!(
                "\n- … {} more skill(s) not listed to keep this catalog short; they remain \
                 invocable with `/<name>` and readable under the skills directories.",
                skills.len() - index
            ));
            break;
        }
        section.push_str(&entry);
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

    fn skill(name: &str, description: &str, path: &str) -> Skill {
        Skill {
            name: name.into(),
            description: description.into(),
            path: path.into(),
            base: "/".into(),
            relative: Path::new(path)
                .strip_prefix("/")
                .unwrap_or(Path::new(path))
                .to_path_buf(),
        }
    }

    #[test]
    fn published_names_and_parsed_names_share_one_grammar() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        write_skill(&repo, ".agents/skills/cafe", "---\nname: café\n---\nbody");
        write_skill(
            &repo,
            ".agents/skills/spaced",
            "---\nname: two words\n---\nbody",
        );
        let skills = discover(None, Some(&repo));
        let names: Vec<_> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            ["café"],
            "names that cannot be typed back are not published"
        );
        assert_eq!(invocation("/café now", &skills).unwrap().arguments, "now");
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_escaping_the_skills_root_are_not_read() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let secret = tmp.path().join("secret.md");
        std::fs::write(&secret, "TOP SECRET").unwrap();
        let escaping = repo.join(".agents/skills/leak");
        std::fs::create_dir_all(&escaping).unwrap();
        std::os::unix::fs::symlink(&secret, escaping.join("SKILL.md")).unwrap();
        // Discovery skips it outright.
        assert!(discover(None, Some(&repo)).is_empty());

        // A file that is swapped for an escaping symlink after discovery is
        // refused at read time too.
        write_skill(&repo, ".agents/skills/ship", "# Ship\n\nreal body");
        let skills = discover(None, Some(&repo));
        assert_eq!(skills.len(), 1);
        std::fs::remove_file(skills[0].path.as_path()).unwrap();
        std::os::unix::fs::symlink(&secret, &skills[0].path).unwrap();
        let block = invocation_block(&invocation("/ship", &skills).unwrap());
        assert!(!block.contains("TOP SECRET"));
        assert!(block.contains("could not be read"));

        // A symlinked skill *directory* is refused as well, even when its
        // target lives inside the skills root: no component may be a link.
        write_skill(&repo, ".agents/skills/real", "# Real\n\nreal body");
        std::os::unix::fs::symlink(
            repo.join(".agents/skills/real"),
            repo.join(".agents/skills/alias"),
        )
        .unwrap();
        let names: Vec<_> = discover(None, Some(&repo))
            .into_iter()
            .map(|skill| skill.name)
            .collect();
        // `ship` is now a symlink too, so only `real` survives.
        assert_eq!(names, ["real"]);
    }

    #[test]
    fn discovery_is_capped_and_deterministic() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        for i in 0..MAX_SKILLS_PER_ROOT + 20 {
            write_skill(&repo, &format!(".agents/skills/s{i:04}"), "body");
        }
        let skills = discover(None, Some(&repo));
        assert_eq!(skills.len(), MAX_SKILLS_PER_ROOT);
        assert_eq!(skills[0].name, "s0000");
        assert_eq!(
            skills.last().unwrap().name,
            format!("s{:04}", MAX_SKILLS_PER_ROOT - 1)
        );
        assert_eq!(discover(None, Some(&repo)), skills);
    }

    #[test]
    fn long_descriptions_and_catalogs_are_bounded() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let long = "d".repeat(MAX_DESCRIPTION_CHARS + 50);
        write_skill(
            &repo,
            ".agents/skills/verbose",
            &format!("---\ndescription: {long}\n---\nbody"),
        );
        let skills = discover(None, Some(&repo));
        assert_eq!(
            skills[0].description.chars().count(),
            MAX_DESCRIPTION_CHARS + 1
        );
        assert!(skills[0].description.ends_with('…'));

        let many: Vec<_> = (0..500)
            .map(|i| {
                skill(
                    &format!("skill-{i:03}"),
                    &"x".repeat(MAX_DESCRIPTION_CHARS),
                    "/tmp/skills/x/SKILL.md",
                )
            })
            .collect();
        let section = prompt_section(&many).unwrap();
        assert!(
            section.len() <= MAX_CATALOG_BYTES + 256,
            "{}",
            section.len()
        );
        assert!(section.contains("more skill(s) not listed"));
        assert!(section.contains("skill-000"));
        assert!(!section.contains("skill-499"));
        assert_eq!(section, prompt_section(&many).unwrap(), "deterministic");
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
        // Far larger than the read cap: only a bounded prefix is loaded.
        let body = "é".repeat(MAX_SKILL_READ_BYTES * 4);
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
        assert!(
            deploy
                .path
                .starts_with(repo.canonicalize().unwrap().join(".agents"))
        );
        let review = skills.iter().find(|s| s.name == "review").unwrap();
        assert_eq!(review.description, "How to review PRs here.");
    }

    #[test]
    fn prompt_section_lists_skills() {
        let skills = vec![skill("write-adr", "Write an ADR", "/x/SKILL.md")];
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
