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

use anyhow::{Result, bail};
use trouve_protocol::{CommandInfo, CommandKind};

const MAX_SKILL_BYTES: u64 = 1024 * 1024;
const MAX_SKILL_DESCRIPTION_CHARS: usize = 512;

struct BuiltInSkill {
    directory: &'static str,
    text: &'static str,
}

const BUILTIN_SKILLS: &[BuiltInSkill] = &[
    BuiltInSkill {
        directory: "code-review",
        text: include_str!("../skills/code-review/SKILL.md"),
    },
    BuiltInSkill {
        directory: "security-review",
        text: include_str!("../skills/security-review/SKILL.md"),
    },
    BuiltInSkill {
        directory: "debug",
        text: include_str!("../skills/debug/SKILL.md"),
    },
    BuiltInSkill {
        directory: "simplify",
        text: include_str!("../skills/simplify/SKILL.md"),
    },
    BuiltInSkill {
        directory: "verify",
        text: include_str!("../skills/verify/SKILL.md"),
    },
    BuiltInSkill {
        directory: "skill-creator",
        text: include_str!("../skills/skill-creator/SKILL.md"),
    },
];

fn valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

fn canonical_skill_root(base: &Path, relative: &Path) -> Option<PathBuf> {
    let base = base.canonicalize().ok()?;
    let root = base.join(relative).canonicalize().ok()?;
    root.starts_with(&base).then_some(root)
}

fn canonical_skill_roots(config_dir: Option<&Path>, workspace_root: Option<&Path>) -> Vec<PathBuf> {
    [
        config_dir.and_then(|dir| canonical_skill_root(dir, Path::new("skills"))),
        workspace_root.and_then(|root| canonical_skill_root(root, Path::new(".agents/skills"))),
    ]
    .into_iter()
    .flatten()
    .collect()
}

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

fn skill_relative_components(path: &Path) -> Result<Vec<&std::ffi::OsStr>> {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(name) => components.push(name),
            _ => bail!(
                "skill path contains an unsafe component: {}",
                path.display()
            ),
        }
    }
    if components.is_empty() {
        bail!("skill path is empty");
    }
    Ok(components)
}

#[cfg(unix)]
fn read_skill_file(root: &Path, relative: &Path) -> Result<String> {
    use std::io::Read as _;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::ffi::OsStrExt as _;

    let relative = skill_relative_components(relative)?;
    if !root.is_absolute() {
        bail!("skill root is not absolute: {}", root.display());
    }
    let anchor = std::ffi::CString::new("/").expect("static path has no NUL");
    let raw = unsafe {
        libc::open(
            anchor.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if raw < 0 {
        return Err(anyhow::anyhow!(
            "opening skill root anchor: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut directory = unsafe { OwnedFd::from_raw_fd(raw) };

    // Walk the canonical root by descriptor. A component swapped for a link
    // after discovery is rejected by O_NOFOLLOW instead of being traversed.
    for component in root.components() {
        let std::path::Component::Normal(name) = component else {
            if matches!(
                component,
                std::path::Component::RootDir | std::path::Component::CurDir
            ) {
                continue;
            }
            bail!(
                "skill root contains an unsafe component: {}",
                root.display()
            );
        };
        let name = std::ffi::CString::new(name.as_bytes())
            .map_err(|_| anyhow::anyhow!("skill root contains NUL"))?;
        let next = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if next < 0 {
            return Err(anyhow::anyhow!(
                "opening skill root without following links: {}",
                std::io::Error::last_os_error()
            ));
        }
        directory = unsafe { OwnedFd::from_raw_fd(next) };
    }

    for component in &relative[..relative.len() - 1] {
        let name = std::ffi::CString::new(component.as_bytes())
            .map_err(|_| anyhow::anyhow!("skill path contains NUL"))?;
        let next = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if next < 0 {
            return Err(anyhow::anyhow!(
                "opening skill directory without following links: {}",
                std::io::Error::last_os_error()
            ));
        }
        directory = unsafe { OwnedFd::from_raw_fd(next) };
    }

    let name = std::ffi::CString::new(relative.last().unwrap().as_bytes())
        .map_err(|_| anyhow::anyhow!("skill path contains NUL"))?;
    let raw = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if raw < 0 {
        return Err(anyhow::anyhow!(
            "opening skill without following links: {}",
            std::io::Error::last_os_error()
        ));
    }
    let file = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(file.as_raw_fd(), metadata.as_mut_ptr()) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let metadata = unsafe { metadata.assume_init() };
    if metadata.st_mode & libc::S_IFMT != libc::S_IFREG {
        bail!("skill is not a regular file");
    }
    if metadata.st_size < 0 || metadata.st_size as u64 > MAX_SKILL_BYTES {
        bail!("skill exceeds the {MAX_SKILL_BYTES} byte limit");
    }
    let mut bytes = Vec::with_capacity(metadata.st_size as usize);
    std::fs::File::from(file)
        .take(MAX_SKILL_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SKILL_BYTES {
        bail!("skill exceeds the {MAX_SKILL_BYTES} byte limit");
    }
    String::from_utf8(bytes).map_err(|error| anyhow::anyhow!("skill is not UTF-8: {error}"))
}

#[cfg(windows)]
fn normalize_windows_skill_path(path: &str) -> String {
    let path = path.replace('/', "\\");
    let path = if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(&path).to_string()
    };
    path.trim_end_matches('\\').to_lowercase()
}

#[cfg(windows)]
fn skill_windows_final_path(file: &std::fs::File) -> Result<String> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
    };
    let handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
    let needed = unsafe {
        GetFinalPathNameByHandleW(
            handle,
            std::ptr::null_mut(),
            0,
            FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
        )
    };
    if needed == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut buffer = vec![0_u16; needed as usize + 1];
    let written = unsafe {
        GetFinalPathNameByHandleW(
            handle,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
        )
    };
    if written == 0 || written as usize >= buffer.len() {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(normalize_windows_skill_path(&String::from_utf16_lossy(
        &buffer[..written as usize],
    )))
}

#[cfg(windows)]
fn read_skill_file(root: &Path, relative: &Path) -> Result<String> {
    use std::io::Read as _;
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let _ = skill_relative_components(relative)?;
    let mut root_options = std::fs::OpenOptions::new();
    root_options
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    let root_file = root_options.open(root)?;
    let root_metadata = root_file.metadata()?;
    if root_metadata.file_attributes() & FILE_ATTRIBUTE_DIRECTORY == 0
        || root_metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        bail!("skill root is not a plain directory");
    }

    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let file = options.open(root.join(relative))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        bail!("skill is not a plain regular file");
    }
    if metadata.len() > MAX_SKILL_BYTES {
        bail!("skill exceeds the {MAX_SKILL_BYTES} byte limit");
    }
    let root_final = skill_windows_final_path(&root_file)?;
    let expected_root = normalize_windows_skill_path(&root.to_string_lossy());
    if root_final != expected_root {
        bail!("opened skill root changed after validation");
    }
    let file_final = skill_windows_final_path(&file)?;
    let suffix = file_final
        .strip_prefix(&root_final)
        .filter(|suffix| suffix.starts_with(['\\', '/']))
        .ok_or_else(|| anyhow::anyhow!("opened skill escaped its declared root"))?;
    if suffix.len() <= 1 {
        bail!("opened skill does not name a file below its declared root");
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_SKILL_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SKILL_BYTES {
        bail!("skill exceeds the {MAX_SKILL_BYTES} byte limit");
    }
    String::from_utf8(bytes).map_err(|error| anyhow::anyhow!("skill is not UTF-8: {error}"))
}

#[cfg(not(any(unix, windows)))]
fn read_skill_file(root: &Path, relative: &Path) -> Result<String> {
    let _ = skill_relative_components(relative)?;
    let path = root.join(relative).canonicalize()?;
    if !path.starts_with(root) {
        bail!("skill escaped its declared root");
    }
    let bytes = std::fs::read(path)?;
    if bytes.len() as u64 > MAX_SKILL_BYTES {
        bail!("skill exceeds the {MAX_SKILL_BYTES} byte limit");
    }
    String::from_utf8(bytes).map_err(|error| anyhow::anyhow!("skill is not UTF-8: {error}"))
}

fn load_dir(base: &Path, relative: &Path, origin: &'static str, out: &mut BTreeMap<String, Skill>) {
    let Some(canonical_dir) = canonical_skill_root(base, relative) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&canonical_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let relative = PathBuf::from(entry.file_name()).join("SKILL.md");
        let Ok(text) = read_skill_file(&canonical_dir, &relative) else {
            continue;
        };
        let directory = entry.file_name().to_string_lossy().to_string();
        if let Some(skill) = skill_from_text(
            &directory,
            &text,
            origin,
            SkillSource::File(canonical_dir.join(relative)),
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
        load_dir(dir, Path::new("skills"), "user", &mut skills);
    }
    if let Some(root) = workspace_root {
        load_dir(root, Path::new(".agents/skills"), "workspace", &mut skills);
    }
    skills.into_values().collect()
}

/// Load one discovered skill by its stable catalog name. Callers never
/// supply a path, so models cannot escape the configured skill roots.
pub fn load(
    config_dir: Option<&Path>,
    workspace_root: Option<&Path>,
    name: &str,
    include_builtins: bool,
) -> Result<(Skill, String)> {
    if name.trim().is_empty() {
        bail!("skill name must not be empty");
    }
    let skill = discover(config_dir, workspace_root, include_builtins)
        .into_iter()
        .find(|skill| skill.name == name)
        .ok_or_else(|| anyhow::anyhow!("unknown skill: {name}"))?;
    match &skill.source {
        SkillSource::BuiltIn(text) => Ok((skill.clone(), (*text).to_string())),
        SkillSource::File(path) => {
            let roots = canonical_skill_roots(config_dir, workspace_root);
            let (root, relative) = roots
                .iter()
                .find_map(|root| {
                    path.strip_prefix(root)
                        .ok()
                        .map(|relative| (root, relative))
                })
                .ok_or_else(|| anyhow::anyhow!("skill {name} escaped its declared root"))?;
            let text = read_skill_file(root, relative)
                .map_err(|error| anyhow::anyhow!("cannot load skill {name}: {error}"))?;
            Ok((skill, text))
        }
    }
}

/// Load a skill through the model-facing tool surface. Explicit user slash
/// commands deliberately use [`load`] instead, because front matter may hide
/// a skill from autonomous model invocation while keeping it user-invocable.
pub fn load_for_model(
    config_dir: Option<&Path>,
    workspace_root: Option<&Path>,
    name: &str,
    include_builtins: bool,
) -> Result<(Skill, String)> {
    let (skill, instructions) = load(config_dir, workspace_root, name, include_builtins)?;
    if skill.disable_model_invocation {
        bail!("skill {name} is not available for model invocation");
    }
    Ok((skill, instructions))
}

/// Build the provider-neutral prompt completion catalog.
pub fn command_catalog(
    config_dir: Option<&Path>,
    workspace_root: Option<&Path>,
    include_builtins: bool,
) -> Vec<CommandInfo> {
    discover(config_dir, workspace_root, include_builtins)
        .into_iter()
        .filter(|skill| skill.user_invocable)
        .map(|skill| CommandInfo {
            usage: if skill.argument_hint.is_empty() {
                format!("/{}", skill.name)
            } else {
                format!("/{} {}", skill.name, skill.argument_hint)
            },
            name: skill.name,
            description: skill.description,
            kind: CommandKind::Prompt,
        })
        .collect()
}

/// Expand an explicit skill invocation into the exact instructions sent to
/// the model while preserving the original text in the user-visible event.
pub fn expand_invocation(
    config_dir: Option<&Path>,
    workspace_root: Option<&Path>,
    input: &str,
    include_builtins: bool,
) -> Result<Option<String>> {
    let Some(after_slash) = input.trim().strip_prefix('/') else {
        return Ok(None);
    };
    let command_end = after_slash
        .find(char::is_whitespace)
        .unwrap_or(after_slash.len());
    let command = &after_slash[..command_end];
    if command.is_empty() {
        return Ok(None);
    }
    let remainder = after_slash[command_end..].trim();
    let (name, request, generic) = if command == "skill" {
        let name_end = remainder
            .find(char::is_whitespace)
            .unwrap_or(remainder.len());
        let name = &remainder[..name_end];
        if name.is_empty() {
            bail!("usage: /skill <name> [request]");
        }
        (name, remainder[name_end..].trim(), true)
    } else {
        (command, remainder, false)
    };
    let Some(skill) = discover(config_dir, workspace_root, include_builtins)
        .into_iter()
        .find(|skill| skill.name == name)
    else {
        if generic {
            bail!("unknown skill: {name}");
        }
        return Ok(None);
    };
    if !skill.user_invocable {
        bail!("skill {name} is not user-invocable");
    }
    let (skill, instructions) = load(config_dir, workspace_root, name, include_builtins)?;
    let request = if request.is_empty() {
        "Apply the explicitly invoked skill to the current task."
    } else {
        request
    };
    Ok(Some(format!(
        "<trouve-skill name=\"{}\">\n{}\n</trouve-skill>\n\n<skill-request>\n{}\n</skill-request>",
        skill.name, instructions, request
    )))
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

    #[cfg(unix)]
    #[test]
    fn discovery_and_load_reject_symlinked_skill_files_and_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let skill_dir = repo.join(".agents/skills/escape");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let outside = tmp.path().join("outside.md");
        std::fs::write(
            &outside,
            "---\nname: escape\ndescription: escaped\n---\nsecret",
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside, skill_dir.join("SKILL.md")).unwrap();

        assert!(
            discover(None, Some(&repo), false)
                .iter()
                .all(|skill| skill.name != "escape")
        );
        assert!(load(None, Some(&repo), "escape", false).is_err());

        let linked_repo = tmp.path().join("linked-repo");
        std::fs::create_dir_all(linked_repo.join(".agents")).unwrap();
        let outside_root = tmp.path().join("outside-skills");
        std::fs::create_dir_all(outside_root.join("escape")).unwrap();
        std::fs::write(
            outside_root.join("escape/SKILL.md"),
            "---\nname: escape\ndescription: escaped\n---\nsecret",
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside_root, linked_repo.join(".agents/skills")).unwrap();
        assert!(
            discover(None, Some(&linked_repo), false)
                .iter()
                .all(|skill| skill.name != "escape")
        );
        assert!(load(None, Some(&linked_repo), "escape", false).is_err());
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
