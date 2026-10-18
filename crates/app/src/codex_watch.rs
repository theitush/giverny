//! App-side Codex state and background usage/metadata requests.
use crossbeam_channel::{Receiver, Sender};
use giverny_claude::{hooks::RelayMsg, wsl};
use giverny_codex::{
    Target, rpc,
    usage::{AccountUsage, Window},
};
use giverny_core::{
    agent::{AgentKind, AgentSession, AgentState},
    config::CodexConfig,
    tabs::TabId,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

pub struct Account {
    pub target: Target,
    pub windows: Vec<Window>,
    pub name: Option<String>,
    pub updated: Option<Instant>,
    pub error: Option<String>,
    pub refreshing: bool,
    pub sampled_at: Option<u64>,
    attempted: Option<Instant>,
}

#[derive(Default)]
pub struct CodexTab {
    pub state: AgentState,
    pub session_id: Option<String>,
    ended: HashSet<String>,
    last_event_at: Option<u64>,
}

pub enum Answer {
    Usage(Target, Result<AccountUsage, String>),
    Target(Target),
    Resume(TabId, String, Result<serde_json::Value, String>),
}

pub struct ReadyResume {
    pub id: TabId,
    pub session_id: String,
    pub command: Vec<u8>,
}

pub struct CodexWatch {
    pub tabs: HashMap<TabId, CodexTab>,
    pub accounts: Vec<Account>,
    pub status: Option<String>,
    pub pending_restore: HashMap<TabId, ReadyResume>,
    pub restoring: HashMap<TabId, String>,
    tx: Sender<(u64, Answer)>,
    rx: Receiver<(u64, Answer)>,
    generation: u64,
    cache_path: Option<PathBuf>,
    config: CodexConfig,
    last_discovery: Instant,
    workers: Vec<(Arc<AtomicBool>, std::thread::JoinHandle<()>)>,
}

impl CodexWatch {
    pub fn new(config: &CodexConfig, cache_path: Option<PathBuf>) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut this = Self {
            tabs: HashMap::new(),
            accounts: Vec::new(),
            status: None,
            pending_restore: HashMap::new(),
            restoring: HashMap::new(),
            tx,
            rx,
            config: config.clone(),
            last_discovery: Instant::now(),
            workers: Vec::new(),
            generation: 0,
            cache_path,
        };
        this.configure(config);
        if let Some(path) = &this.cache_path {
            for snapshot in giverny_codex::usage::load_cache(path) {
                if let Some(a) = this
                    .accounts
                    .iter_mut()
                    .find(|a| a.target == snapshot.target)
                {
                    let age = unix_now().saturating_sub(snapshot.sampled_at);
                    a.windows = snapshot.windows;
                    a.name = snapshot.account_name;
                    a.sampled_at = Some(snapshot.sampled_at);
                    a.updated = Instant::now().checked_sub(Duration::from_secs(age));
                }
            }
        }
        this
    }

    pub fn configure(&mut self, config: &CodexConfig) {
        let target_changed =
            self.config.executable != config.executable || self.config.home != config.home;
        if target_changed || self.config.enabled != config.enabled {
            self.generation = self.generation.wrapping_add(1);
            for (cancel, _) in &self.workers {
                cancel.store(true, Ordering::Relaxed);
            }
            for account in &mut self.accounts {
                account.refreshing = false;
            }
            self.restoring.clear();
            self.pending_restore.clear();
        }
        if self.config.enabled != config.enabled {
            for a in &mut self.accounts {
                a.attempted = None;
            }
        }
        self.config = config.clone();
        self.last_discovery = Instant::now();
        if !config.enabled {
            self.tabs.clear();
            return;
        }
        if target_changed {
            self.accounts.clear()
        }
        let mut target = Target::native(&config.executable, &config.home);
        if let Some((distro, home)) = wsl::split_unc(&target.home) {
            target.wsl_distro = Some(distro);
            target.home = home.into();
        }
        if target.is_detected() || !config.home.is_empty() || config.executable != "codex" {
            self.add_target(target);
        }
        #[cfg(windows)]
        {
            let tx = self.tx.clone();
            let executable = config.executable.clone();
            let generation = self.generation;
            std::thread::spawn(move || {
                for distro in wsl::distros() {
                    if let Some(home) = wsl::home(&distro) {
                        let home = format!("{home}/.codex");
                        if wsl::unc_path(&distro, &home).is_dir() {
                            let _ = tx.send((
                                generation,
                                Answer::Target(Target {
                                    executable: executable.clone(),
                                    home: home.into(),
                                    wsl_distro: Some(distro),
                                }),
                            ));
                        }
                    }
                }
            });
        }
    }

    fn add_target(&mut self, target: Target) {
        if self.accounts.iter().all(|a| a.target != target) {
            self.accounts.push(Account {
                target,
                windows: Vec::new(),
                name: None,
                updated: None,
                error: None,
                refreshing: false,
                attempted: None,
                sampled_at: None,
            });
        }
    }

    pub fn hook_path(target: &Target) -> PathBuf {
        match &target.wsl_distro {
            Some(d) => wsl::unc_path(d, &target.home.join("hooks.json").to_string_lossy()),
            None => target.home.join("hooks.json"),
        }
    }

    pub fn hook_command(target: &Target) -> String {
        let relay = giverny_claude::hooks::relay_command_for(&Self::hook_path(target));
        // The installer is the source of the home: Windows relay subprocesses
        // must not reinterpret a Linux CODEX_HOME as a Windows path.
        let home = target.home.to_string_lossy();
        let quoted = if cfg!(windows) && target.wsl_distro.is_none() {
            format!("\"{}\"", home.replace('"', "\"\""))
        } else {
            giverny_codex::quote(&home)
        };
        let distro = target
            .wsl_distro
            .as_ref()
            .map(|d| format!(" --wsl-distro {}", giverny_codex::quote(d)))
            .unwrap_or_default();
        format!("{relay} --provider codex --home {quoted}{distro}")
    }

    pub fn install_hooks(&mut self, target: &Target, enable: bool) {
        let result = giverny_codex::hooks::set_installed(
            &Self::hook_path(target),
            &Self::hook_command(target),
            enable,
        );
        self.status=Some(match result {
            Ok(()) if enable => "Hooks installed. Open /hooks in Codex to review and trust them, then restart the session.".into(),
            Ok(()) => "Giverny's Codex hooks removed; other hooks were preserved.".into(),
            Err(e) => format!("Codex hooks: {e:#}"),
        });
    }

    pub fn state_of(&self, id: TabId) -> AgentState {
        self.tabs.get(&id).map(|t| t.state).unwrap_or_default()
    }
    pub fn attended(&mut self, id: TabId) {
        if let Some(t) = self.tabs.get_mut(&id)
            && matches!(t.state, AgentState::NeedsYou | AgentState::DoneUnseen)
        {
            t.state = AgentState::Idle
        }
    }

    /// Returns persistence updates separately from presentation. Unknown or
    /// late events cannot replace a newer session's identity.
    pub fn handle(
        &mut self,
        msg: &RelayMsg,
        id: TabId,
        distro: Option<String>,
        active: bool,
    ) -> Option<Option<AgentSession>> {
        if !self.config.enabled || msg.provider != "codex" || msg.agent_id().is_some() {
            return None;
        }
        let sid = msg.session_id()?;
        if !giverny_codex::valid_session_id(sid) {
            return None;
        }
        let home = msg
            .config_dir
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| Target::native(&self.config.executable, &self.config.home).home);
        let target = Target {
            executable: self.config.executable.clone(),
            home: home.clone(),
            wsl_distro: distro.clone(),
        };
        self.add_target(target);
        let tab = self.tabs.entry(id).or_default();
        if msg
            .emitted_at
            .zip(tab.last_event_at)
            .is_some_and(|(at, last)| at < last)
        {
            return None;
        }
        if msg.hook_event() == Some("SessionStart") {
            tab.ended.remove(sid);
        } else if tab.ended.contains(sid) {
            return None;
        }
        if msg.emitted_at.is_some() {
            tab.last_event_at = msg.emitted_at;
        }

        if msg.hook_event() != Some("SessionStart")
            && tab.session_id.as_deref().is_some_and(|s| s != sid)
        {
            return None;
        }
        let capture = matches!(msg.hook_event(), Some("SessionStart" | "UserPromptSubmit"))
            || tab.session_id.is_none();
        tab.session_id = Some(sid.into());
        tab.state = match msg.hook_event() {
            Some("SessionStart" | "Interrupt") => AgentState::Idle,
            Some("PreToolUse")
                if matches!(
                    msg.event.get("tool_name").and_then(|v| v.as_str()),
                    Some("AskUserQuestion" | "request_user_input")
                ) =>
            {
                AgentState::NeedsYou
            }
            Some("UserPromptSubmit" | "PreToolUse" | "PostToolUse") => AgentState::Busy,
            Some("PermissionRequest") => AgentState::NeedsYou,
            Some("Stop") => {
                if active {
                    AgentState::Idle
                } else {
                    AgentState::DoneUnseen
                }
            }
            Some("SessionEnd") => {
                tab.ended.insert(sid.into());
                tab.session_id = None;
                tab.state = AgentState::None;
                return Some(None);
            }
            _ => return None,
        };
        if capture {
            let cwd = msg.event.get("cwd").and_then(|v| v.as_str())?;
            Some(Some(AgentSession {
                provider: AgentKind::Codex,
                id: sid.into(),
                home,
                cwd: cwd.into(),
                wsl_distro: distro,
            }))
        } else {
            None
        }
    }

    pub fn refresh(&mut self, force: bool, ctx: &egui::Context) {
        if !self.config.enabled {
            return;
        }
        let interval = Duration::from_secs(self.config.usage_refresh_minutes.saturating_mul(60));
        for account in &mut self.accounts {
            if account.refreshing {
                continue;
            }
            if !force
                && (interval.is_zero()
                    || account.attempted.is_some_and(|at| at.elapsed() < interval))
            {
                continue;
            }
            account.attempted = Some(Instant::now());
            account.refreshing = true;
            let tx = self.tx.clone();
            let target = account.target.clone();
            let wake = ctx.clone();
            let generation = self.generation;
            let cancel = Arc::new(AtomicBool::new(false));
            let task_cancel = cancel.clone();
            let result = std::thread::Builder::new()
                .name("giverny codex usage".into())
                .spawn(move || {
                    let result =
                        rpc::fetch_usage(&target, task_cancel).map_err(|e| format!("{e:#}"));
                    let _ = tx.send((generation, Answer::Usage(target, result)));
                    wake.request_repaint();
                });
            match result {
                Ok(handle) => self.workers.push((cancel, handle)),
                Err(e) => {
                    account.refreshing = false;
                    account.error = Some(e.to_string())
                }
            }
        }
    }

    pub fn inspect_restore(&mut self, id: TabId, session: &AgentSession, ctx: &egui::Context) {
        if !self.config.enabled
            || !giverny_codex::valid_session_id(&session.id)
            || self.restoring.contains_key(&id)
        {
            return;
        }
        let target = Target {
            executable: self.config.executable.clone(),
            home: session.home.clone(),
            wsl_distro: session.wsl_distro.clone(),
        };
        let sid = session.id.clone();
        self.restoring.insert(id, sid.clone());
        let tx = self.tx.clone();
        let wake = ctx.clone();
        let generation = self.generation;
        let cancel = Arc::new(AtomicBool::new(false));
        let task_cancel = cancel.clone();
        let result = std::thread::Builder::new()
            .name("giverny codex restore".into())
            .spawn(move || {
                let result =
                    rpc::inspect_session(&target, &sid, task_cancel).map_err(|e| format!("{e:#}"));
                let _ = tx.send((generation, Answer::Resume(id, sid, result)));
                wake.request_repaint();
            });
        match result {
            Ok(handle) => self.workers.push((cancel, handle)),
            Err(e) => {
                self.restoring.remove(&id);
                self.status = Some(e.to_string())
            }
        }
    }

    pub fn answers(&mut self) -> Vec<Answer> {
        if self.config.enabled && self.last_discovery.elapsed() >= Duration::from_secs(30) {
            self.configure(&self.config.clone());
        }
        self.workers.retain(|(_, h)| !h.is_finished());
        let mut resumes = Vec::new();
        let mut cache_changed = false;
        for (generation, answer) in self.rx.try_iter().collect::<Vec<_>>() {
            if generation != self.generation {
                continue;
            }
            match answer {
                Answer::Target(target) => self.add_target(target),
                Answer::Usage(target, result) => {
                    if let Some(a) = self.accounts.iter_mut().find(|a| a.target == target) {
                        a.refreshing = false;
                        match result {
                            Ok(w) => {
                                a.windows = w.windows;
                                a.name = w.name;
                                a.updated = Some(Instant::now());
                                a.sampled_at = Some(unix_now());
                                a.error = None;
                                cache_changed = true
                            }
                            Err(e) => a.error = Some(e),
                        }
                    }
                }
                Answer::Resume(..) => resumes.push(answer),
            }
        }
        if cache_changed && let Some(path) = &self.cache_path {
            let snapshots: Vec<_> = self
                .accounts
                .iter()
                .filter_map(|a| {
                    a.sampled_at
                        .map(|sampled_at| giverny_codex::usage::Snapshot {
                            target: a.target.clone(),
                            account_name: a.name.clone(),
                            windows: a.windows.clone(),
                            sampled_at,
                        })
                })
                .collect();
            if let Err(e) = giverny_codex::usage::save_cache(path, &snapshots) {
                tracing::warn!("Codex usage cache: {e:#}");
            }
        }
        resumes
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl Drop for CodexWatch {
    fn drop(&mut self) {
        for (cancel, _) in &self.workers {
            cancel.store(true, Ordering::Relaxed);
        }
        for (_, handle) in self.workers.drain(..) {
            let _ = handle.join();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ResumeShell {
    Posix,
    PowerShell,
    Cmd,
}

/// Build only known resume commands, using quoting for the tab's shell.
pub fn resume_command(
    target: &Target,
    id: &str,
    cwd: &Path,
    shell: ResumeShell,
) -> Option<Vec<u8>> {
    if !giverny_codex::valid_session_id(id) {
        return None;
    }
    let home = target.home.to_string_lossy();
    let cwd = cwd.to_string_lossy();
    let quote = |s: &str| {
        if shell == ResumeShell::PowerShell {
            format!("'{}'", s.replace('\'', "''"))
        } else {
            giverny_codex::quote(s)
        }
    };
    if [home.as_ref(), cwd.as_ref(), target.executable.as_str()]
        .iter()
        .any(|s| s.chars().any(char::is_control))
    {
        return None;
    }
    if shell == ResumeShell::Cmd {
        // Percent expansion and delayed expansion occur even inside quotes.
        // Refuse those uncommon filenames rather than reinterpret them.
        if [home.as_ref(), cwd.as_ref(), target.executable.as_str()]
            .iter()
            .any(|s| s.contains(['%', '!', '"', '^']))
        {
            return None;
        }
        return Some(
            format!(
                "set \"CODEX_HOME={}\" && \"{}\" resume {} --cd \"{}\"\r",
                home, target.executable, id, cwd
            )
            .into_bytes(),
        );
    }
    Some(
        if shell == ResumeShell::PowerShell {
            format!(
                "$env:CODEX_HOME={}; & {} resume {} --cd {}\r",
                quote(&home),
                quote(&target.executable),
                quote(id),
                quote(&cwd)
            )
        } else {
            // export works in bash/zsh and fish, unlike leading NAME=value on old fish.
            format!(
                "env CODEX_HOME={} {} resume {} --cd {}\r",
                quote(&home),
                quote(&target.executable),
                quote(id),
                quote(&cwd)
            )
        }
        .into_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn hook(event: &str, sid: &str) -> RelayMsg {
        serde_json::from_value(serde_json::json!({"provider":"codex","tab_id":"giverny-1","config_dir":"/home/test/.codex","event":{"session_id":sid,"hook_event_name":event,"cwd":"/tmp/project"}})).unwrap()
    }
    #[test]
    fn same_directory_does_not_mix_tabs_and_stale_events_are_ignored() {
        let mut watch = CodexWatch::new(
            &CodexConfig {
                enabled: true,
                ..Default::default()
            },
            None,
        );
        let a = "01a144ed-3193-79d3-9569-4b9e28f3d9e4";
        let b = "01a144ed-549c-7a82-8fc0-f156c3643c99";
        watch.handle(&hook("SessionStart", a), TabId(1), None, false);
        watch.handle(&hook("SessionStart", b), TabId(2), None, false);
        watch.handle(&hook("UserPromptSubmit", a), TabId(1), None, false);
        watch.handle(&hook("PermissionRequest", b), TabId(2), None, false);
        assert_eq!(watch.state_of(TabId(1)), AgentState::Busy);
        assert_eq!(watch.state_of(TabId(2)), AgentState::NeedsYou);
        watch.handle(&hook("Stop", a), TabId(2), None, false);
        assert_eq!(watch.state_of(TabId(2)), AgentState::NeedsYou);
        watch.handle(&hook("Stop", a), TabId(1), None, false);
        assert_eq!(watch.state_of(TabId(1)), AgentState::DoneUnseen);
        watch.attended(TabId(1));
        assert_eq!(watch.state_of(TabId(1)), AgentState::Idle);
        watch.handle(&hook("Interrupt", b), TabId(2), None, false);
        assert_eq!(watch.state_of(TabId(2)), AgentState::Idle);
        watch.handle(&hook("SessionEnd", a), TabId(1), None, false);
        assert_eq!(watch.state_of(TabId(1)), AgentState::None);
    }
    #[test]
    fn quotes_restore_arguments_for_both_shells() {
        let target = Target {
            executable: "/a b/codex".into(),
            home: "/a'b/$home".into(),
            wsl_distro: None,
        };
        let id = "01a144ed-3193-79d3-9569-4b9e28f3d9e4";
        let command = String::from_utf8(
            resume_command(
                &target,
                id,
                Path::new("/project;touch x"),
                ResumeShell::Posix,
            )
            .unwrap(),
        )
        .unwrap();
        assert!(command.contains("'/a'\\''b/$home'"));
        assert!(command.contains("'/project;touch x'"));
        let command = String::from_utf8(
            resume_command(&target, id, Path::new("C:\\a'b"), ResumeShell::PowerShell).unwrap(),
        )
        .unwrap();
        assert!(command.contains("'C:\\a''b'"));
    }
    #[test]
    fn disabled_integration_never_launches_a_usage_helper() {
        let mut watch = CodexWatch::new(
            &CodexConfig {
                enabled: false,
                executable: "does-not-exist".into(),
                ..Default::default()
            },
            None,
        );
        watch.refresh(true, &egui::Context::default());
        assert!(watch.workers.is_empty());
        assert!(watch.accounts.iter().all(|a| !a.refreshing));
    }

    #[test]
    fn old_worker_results_cannot_overwrite_reconfigured_accounts() {
        let cfg = CodexConfig {
            enabled: true,
            home: std::env::temp_dir().display().to_string(),
            ..Default::default()
        };
        let mut watch = CodexWatch::new(&cfg, None);
        let old = watch.generation;
        let target = watch.accounts[0].target.clone();
        watch.configure(&CodexConfig {
            enabled: false,
            ..cfg.clone()
        });
        watch.configure(&cfg);
        watch.accounts[0].refreshing = true;
        watch
            .tx
            .send((old, Answer::Usage(target, Err("old error".into()))))
            .unwrap();
        watch.answers();
        assert!(watch.accounts[0].refreshing);
        assert!(watch.accounts[0].error.is_none());
    }

    #[test]
    fn timestamps_subagents_and_ended_sessions_do_not_revive_stale_state() {
        let mut watch = CodexWatch::new(
            &CodexConfig {
                enabled: true,
                ..Default::default()
            },
            None,
        );
        let sid = "01a144ed-3193-79d3-9569-4b9e28f3d9e4";
        let mut start = hook("SessionStart", sid);
        start.emitted_at = Some(100);
        watch.handle(&start, TabId(1), None, false);
        let mut prompt = hook("UserPromptSubmit", sid);
        prompt.emitted_at = Some(200);
        watch.handle(&prompt, TabId(1), None, false);
        let mut stop = hook("Stop", sid);
        stop.emitted_at = Some(150);
        watch.handle(&stop, TabId(1), None, false);
        assert_eq!(watch.state_of(TabId(1)), AgentState::Busy);
        stop.emitted_at = Some(250);
        stop.event["agent_id"] = "child".into();
        watch.handle(&stop, TabId(1), None, false);
        assert_eq!(watch.state_of(TabId(1)), AgentState::Busy);
        let mut end = hook("SessionEnd", sid);
        end.emitted_at = Some(300);
        watch.handle(&end, TabId(1), None, false);
        stop.event.as_object_mut().unwrap().remove("agent_id");
        stop.emitted_at = Some(400);
        watch.handle(&stop, TabId(1), None, false);
        assert_eq!(watch.state_of(TabId(1)), AgentState::None);
    }

    #[test]
    fn cmd_resume_refuses_expansion_characters() {
        let target = Target {
            executable: "codex".into(),
            home: "C:\\Users\\test\\.codex".into(),
            wsl_distro: None,
        };
        let id = "01a144ed-3193-79d3-9569-4b9e28f3d9e4";
        assert!(resume_command(&target, id, Path::new("C:\\a b"), ResumeShell::Cmd).is_some());
        assert!(resume_command(&target, id, Path::new("C:\\%PATH%"), ResumeShell::Cmd).is_none());
    }
}
