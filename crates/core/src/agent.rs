//! Shared agent presentation and persisted session identity.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    #[default]
    Claude,
    Codex,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentState {
    #[default]
    None,
    Idle,
    Busy,
    NeedsYou,
    DoneUnseen,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSession {
    pub provider: AgentKind,
    pub id: String,
    pub home: PathBuf,
    pub cwd: PathBuf,
    #[serde(default)]
    pub wsl_distro: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_claude_tabs_and_provider_tagged_sessions_round_trip() {
        let mut ws = crate::tabs::Workspace::default();
        let id = ws.add_tab(ws.categories[0].id);
        ws.tab_mut(id).unwrap().claude_session = Some("legacy-session".into());
        let mut value = serde_json::to_value(&ws).unwrap();
        value["tabs"][0]
            .as_object_mut()
            .unwrap()
            .remove("agent_session");
        let restored: crate::tabs::Workspace = serde_json::from_value(value).unwrap();
        assert_eq!(
            restored.tab(id).unwrap().claude_session.as_deref(),
            Some("legacy-session")
        );
        assert!(restored.tab(id).unwrap().agent_session.is_none());
        ws.tab_mut(id).unwrap().agent_session = Some(AgentSession {
            provider: AgentKind::Codex,
            id: "session".into(),
            home: "/home/test/.codex".into(),
            cwd: "/work/project".into(),
            wsl_distro: Some("Debian".into()),
        });
        let restored: crate::tabs::Workspace =
            serde_json::from_str(&serde_json::to_string(&ws).unwrap()).unwrap();
        let session = restored.tab(id).unwrap().agent_session.as_ref().unwrap();
        assert_eq!(session.provider, AgentKind::Codex);
        assert_eq!(session.wsl_distro.as_deref(), Some("Debian"));
        assert!(crate::config::Config::default().codex.enabled);
    }
}
