//! Provider-neutral slash-command catalog.
//!
//! Prompt commands are sent through the normal turn path. Action commands
//! are interpreted by [`crate::engine::Engine`] and never reach a model.

use std::collections::HashSet;

use trouve_protocol::{CommandInfo, CommandKind};

#[derive(Debug, Clone, Copy)]
pub struct CommandSpec {
    pub name: &'static str,
    pub usage: &'static str,
    pub description: &'static str,
    pub kind: CommandKind,
}

const CORE_COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "help",
        usage: "/help [command]",
        description: "List commands or show help for one command.",
        kind: CommandKind::Action,
    },
    CommandSpec {
        name: "status",
        usage: "/status",
        description: "Show the current session, thread, provider, and turn state.",
        kind: CommandKind::Action,
    },
    CommandSpec {
        name: "skills",
        usage: "/skills [name]",
        description: "List available skills or inspect one skill.",
        kind: CommandKind::Action,
    },
    CommandSpec {
        name: "skill",
        usage: "/skill <name> [request]",
        description: "Explicitly invoke a skill for a model task.",
        kind: CommandKind::Prompt,
    },
    CommandSpec {
        name: "mode",
        usage: "/mode [id]",
        description: "List modes, show the current mode, or switch modes.",
        kind: CommandKind::Action,
    },
    CommandSpec {
        name: "model",
        usage: "/model [provider/model]",
        description: "List models, show the current model, or switch models.",
        kind: CommandKind::Action,
    },
    CommandSpec {
        name: "permissions",
        usage: "/permissions [ask|allow-list|yolo]",
        description: "Show or change the thread's permission policy.",
        kind: CommandKind::Action,
    },
    CommandSpec {
        name: "undo",
        usage: "/undo",
        description: "Restore the session's previous checkpoint.",
        kind: CommandKind::Action,
    },
    CommandSpec {
        name: "redo",
        usage: "/redo",
        description: "Restore the next checkpoint after an undo.",
        kind: CommandKind::Action,
    },
    CommandSpec {
        name: "cancel",
        usage: "/cancel",
        description: "Cancel the current model turn.",
        kind: CommandKind::Action,
    },
    CommandSpec {
        name: "new",
        usage: "/new",
        description: "Create and switch to a new thread in this session.",
        kind: CommandKind::Action,
    },
    CommandSpec {
        name: "tools",
        usage: "/tools",
        description: "List the Trouve tools available to this thread.",
        kind: CommandKind::Action,
    },
    CommandSpec {
        name: "mcp",
        usage: "/mcp",
        description: "Show the MCP servers resolved for this session.",
        kind: CommandKind::Action,
    },
    CommandSpec {
        name: "usage",
        usage: "/usage",
        description: "Show accumulated token and cost usage for this thread.",
        kind: CommandKind::Action,
    },
    CommandSpec {
        name: "diff",
        usage: "/diff",
        description: "Show the session's diff against its base revision.",
        kind: CommandKind::Action,
    },
    CommandSpec {
        name: "files",
        usage: "/files",
        description: "List files in the session worktree.",
        kind: CommandKind::Action,
    },
    CommandSpec {
        name: "queue",
        usage: "/queue",
        description: "Show prompts waiting on this thread.",
        kind: CommandKind::Action,
    },
    CommandSpec {
        name: "instructions",
        usage: "/instructions",
        description: "Show the effective Trouve instructions for this thread.",
        kind: CommandKind::Action,
    },
    CommandSpec {
        name: "rename",
        usage: "/rename <title>",
        description: "Rename the current session.",
        kind: CommandKind::Action,
    },
    CommandSpec {
        name: "terminal",
        usage: "/terminal",
        description: "Open the current session's integrated terminal.",
        kind: CommandKind::Action,
    },
];

pub fn spec(name: &str) -> Option<&'static CommandSpec> {
    CORE_COMMANDS.iter().find(|command| command.name == name)
}

pub fn action_spec(name: &str) -> Option<&'static CommandSpec> {
    spec(name).filter(|command| command.kind == CommandKind::Action)
}

pub fn catalog(skill_commands: Vec<CommandInfo>) -> Vec<CommandInfo> {
    let mut commands: Vec<_> = CORE_COMMANDS
        .iter()
        .map(|command| CommandInfo {
            name: command.name.into(),
            description: command.description.into(),
            kind: command.kind,
            usage: command.usage.into(),
        })
        .collect();
    let reserved: HashSet<_> = CORE_COMMANDS.iter().map(|command| command.name).collect();
    commands.extend(
        skill_commands
            .into_iter()
            .filter(|command| !reserved.contains(command.name.as_str())),
    );
    commands
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_command_waves_are_typed_and_stably_ordered() {
        let commands = catalog(Vec::new());
        assert_eq!(commands.len(), 20);
        assert_eq!(commands.first().unwrap().name, "help");
        assert_eq!(commands.last().unwrap().name, "terminal");
        assert_eq!(spec("skill").unwrap().kind, CommandKind::Prompt);
        assert_eq!(action_spec("status").unwrap().usage, "/status");
        assert!(action_spec("skill").is_none());
    }

    #[test]
    fn core_names_win_direct_skill_collisions() {
        let commands = catalog(vec![CommandInfo {
            name: "status".into(),
            description: "A colliding skill".into(),
            kind: CommandKind::Prompt,
            usage: "/status".into(),
        }]);
        assert_eq!(
            commands
                .iter()
                .filter(|command| command.name == "status")
                .count(),
            1
        );
        assert_eq!(
            commands
                .iter()
                .find(|command| command.name == "status")
                .unwrap()
                .kind,
            CommandKind::Action
        );
    }
}
