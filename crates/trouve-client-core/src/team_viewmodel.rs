//! Folded client state for a session-scoped agent team.

use trouve_protocol::{Event, EventEnvelope, Team, TeamMember, TeamMessage};

#[derive(Debug, Clone, Default)]
pub struct TeamViewModel {
    pub team: Option<Team>,
    pub cursor: u64,
}

impl TeamViewModel {
    pub fn from_team(team: Team) -> Self {
        let cursor = team.snapshot_cursor;
        Self {
            team: Some(team),
            cursor,
        }
    }

    pub fn apply(&mut self, envelope: &EventEnvelope) {
        if envelope.cursor <= self.cursor {
            return;
        }
        self.cursor = envelope.cursor;
        match &envelope.event {
            Event::TeamCreated { team } => {
                let mut team = team.clone();
                team.snapshot_cursor = envelope.cursor;
                self.team = Some(team);
            }
            Event::TeamMessagePosted { message } => {
                if let Some(team) = &mut self.team {
                    upsert_message(&mut team.messages, message);
                }
            }
            Event::TeamMemberUpdated { member } => {
                if let Some(team) = &mut self.team {
                    upsert_member(&mut team.members, member);
                }
            }
            Event::TeamStatusChanged { status, turns_used } => {
                if let Some(team) = &mut self.team {
                    team.status = *status;
                    team.turns_used = *turns_used;
                }
            }
            _ => {}
        }
        if let Some(team) = &mut self.team {
            team.snapshot_cursor = self.cursor;
        }
    }
}

fn upsert_message(messages: &mut Vec<TeamMessage>, message: &TeamMessage) {
    if let Some(existing) = messages.iter_mut().find(|item| item.id == message.id) {
        *existing = message.clone();
    } else {
        messages.push(message.clone());
    }
}

fn upsert_member(members: &mut Vec<TeamMember>, member: &TeamMember) {
    if let Some(existing) = members.iter_mut().find(|item| item.id == member.id) {
        *existing = member.clone();
    } else {
        members.push(member.clone());
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use trouve_protocol::{Scope, TeamStatus};

    use super::*;

    #[test]
    fn snapshot_cursor_skips_stale_creation_and_folds_newer_status() {
        let team = Team {
            session_id: "se_1".into(),
            snapshot_cursor: 5,
            goal: "ship it".into(),
            status: TeamStatus::Active,
            orchestrator_member_id: "tm_1".into(),
            members: vec![],
            messages: vec![],
            max_turns: 8,
            turns_used: 0,
            created_at: Utc::now(),
        };
        let mut stale = team.clone();
        stale.snapshot_cursor = 0;
        stale.goal = "stale initial goal".into();
        let mut vm = TeamViewModel::from_team(team);
        vm.apply(&EventEnvelope {
            cursor: 1,
            scope: Scope::Session("se_1".into()),
            ts: Utc::now(),
            event: Event::TeamCreated { team: stale },
        });
        vm.apply(&EventEnvelope {
            cursor: 6,
            scope: Scope::Session("se_1".into()),
            ts: Utc::now(),
            event: Event::TeamStatusChanged {
                status: TeamStatus::Paused,
                turns_used: 3,
            },
        });
        let folded = vm.team.unwrap();
        assert_eq!(folded.goal, "ship it");
        assert_eq!(folded.status, TeamStatus::Paused);
        assert_eq!(folded.turns_used, 3);
        assert_eq!(folded.snapshot_cursor, 6);
    }
}
