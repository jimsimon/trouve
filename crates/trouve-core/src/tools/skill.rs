//! Provider-neutral skill loading by catalog name.

use serde_json::{Value, json};

use super::{Tool, ToolCtx, ToolResult};

pub struct LoadSkill;

#[async_trait::async_trait]
impl Tool for LoadSkill {
    fn name(&self) -> &'static str {
        "load_skill"
    }

    fn description(&self) -> &'static str {
        "Load a Trouve skill's complete instructions by its advertised name. Call this before \
         using a relevant skill; paths are intentionally not accepted."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Exact skill name from the available-skills catalog"
                }
            },
            "required": ["name"],
            "additionalProperties": false
        })
    }

    fn mutates(&self) -> bool {
        false
    }

    async fn run(&self, ctx: &ToolCtx, args: &Value) -> ToolResult {
        let Some(name) = args.get("name").and_then(Value::as_str) else {
            return ToolResult::error("missing required argument: name");
        };
        match crate::skills::load(
            ctx.config_dir.as_deref(),
            ctx.workspace_root.as_deref(),
            name,
            ctx.builtin_skills_enabled,
        ) {
            Ok((skill, instructions)) => ToolResult::ok(json!({
                "name": skill.name,
                "description": skill.description,
                "instructions": instructions,
            })),
            Err(e) => ToolResult::error(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;

    #[tokio::test]
    async fn loads_a_workspace_skill_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let worktree = tmp.path().join("worktree");
        std::fs::create_dir_all(repo.join(".agents/skills/review")).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(
            repo.join(".agents/skills/review/SKILL.md"),
            "---\nname: review\ndescription: Review changes\n---\n\nCheck the diff.",
        )
        .unwrap();
        let ctx = ToolCtx {
            worktree,
            config_dir: None,
            workspace_root: Some(repo),
            builtin_skills_enabled: true,
            ..ToolCtx::default()
        };

        let result = LoadSkill.run(&ctx, &json!({"name": "review"})).await;
        assert_eq!(result.status, trouve_protocol::ToolStatus::Ok);
        assert_eq!(result.result["name"], "review");
        assert!(
            result.result["instructions"]
                .as_str()
                .unwrap()
                .contains("Check the diff.")
        );
    }

    #[tokio::test]
    async fn disabled_builtins_cannot_be_loaded_but_workspace_skills_can() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join(".agents/skills/review")).unwrap();
        std::fs::write(
            repo.join(".agents/skills/review/SKILL.md"),
            "---\nname: review\ndescription: Review changes\n---\n\nReview it.",
        )
        .unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            config_dir: None,
            workspace_root: Some(repo),
            builtin_skills_enabled: false,
            ..ToolCtx::default()
        };

        let builtin = LoadSkill.run(&ctx, &json!({"name": "code-review"})).await;
        assert_eq!(builtin.status, trouve_protocol::ToolStatus::Error);

        let workspace = LoadSkill.run(&ctx, &json!({"name": "review"})).await;
        assert_eq!(workspace.status, trouve_protocol::ToolStatus::Ok);
    }
}
