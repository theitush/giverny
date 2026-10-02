//! `~/.config/giverny/config.toml` — user settings, written with comments on
//! first run and hot-reloaded when the file changes.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub font: FontConfig,
    pub theme: ThemeConfig,
    pub titles: TitlesConfig,
    pub behavior: BehaviorConfig,
    pub usage: UsageConfig,
    pub claude: ClaudeConfig,
    pub update: UpdateConfig,
}

/// How Claude Code itself is launched in a tab.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ClaudeConfig {
    /// Start every session in auto mode, by setting `permissions.defaultMode`
    /// in each account's `settings.json`.
    pub auto_mode: bool,
    /// Suppress Claude Code's "resume from summary / resume full session
    /// as-is" prompt, so a resumed conversation comes back whole.
    pub skip_resume_summary: bool,
    /// Pick a session back up when the usage window that stopped it reopens.
    pub resume_after_limit: bool,
    /// Show the tab's subagents in a table under the terminal.
    pub agents_pane: bool,
    /// Tell every new Claude session to run work longer than about a minute
    /// as an orchestrator pass of subagents (needs `agents_pane`).
    pub orchestrate_by_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TitlesConfig {
    /// Drop a leading `user@host:` from titles the shell sets.
    pub strip_host_prefix: bool,
    /// Abbreviate every directory but the last: `~/Dev/bobo` → `~/D/bobo`.
    pub shorten_paths: bool,
}

impl Default for TitlesConfig {
    fn default() -> Self {
        TitlesConfig {
            strip_host_prefix: true,
            shorten_paths: false,
        }
    }
}

/// Tidy a title the shell set, for a rail that is 240px wide.
///
/// Applied at *display* time, never to the stored title: toggling either
/// option then takes effect on every existing tab at once, instead of only on
/// titles set afterwards.
pub fn display_title(raw: &str, cfg: &TitlesConfig) -> String {
    let mut out = raw;
    if cfg.strip_host_prefix {
        out = strip_host_prefix(out);
    }
    if let Some(program) = program_title(out) {
        return program;
    }
    if cfg.shorten_paths {
        return shorten_paths(out);
    }
    out.to_string()
}

/// A title that is nothing but the path to the program is that program.
///
/// Windows sets a console's title to the command line that opened it, and
/// ConPTY passes that on, so a PowerShell tab announces itself as
/// `C:\WINDOWS\System32\WindowsPowerShell\v1.0\powershell.exe` — a rail's
/// width of path saying one word. Unconditional: no `[titles]` option makes a
/// tab want to be called that.
fn program_title(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let name = trimmed.rsplit(['\\', '/']).next()?;
    let stem = name
        .strip_suffix(".exe")
        .or_else(|| name.strip_suffix(".EXE"))?;
    // Only a bare path: anything with arguments is a title someone chose.
    (!stem.is_empty() && !trimmed.contains(char::is_whitespace)).then(|| stem.to_string())
}

/// `yoz@yoz-framework:~/Dev/bobo` → `~/Dev/bobo`.
///
/// Narrow on purpose: only `name@host:` at the very start, where both parts
/// look like a name. `ssh: user@host` and titles that merely contain an `@`
/// are left alone.
fn strip_host_prefix(title: &str) -> &str {
    let Some(colon) = title.find(':') else {
        return title;
    };
    let (prefix, rest) = title.split_at(colon);
    let Some((user, host)) = prefix.split_once('@') else {
        return title;
    };
    let plain = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_'))
    };
    if plain(user) && plain(host) {
        rest[1..].trim_start()
    } else {
        title
    }
}

/// `~/Dev/claude_test/giverny` → `~/D/c/giverny`. Only the last segment keeps
/// its name — the one you are actually in.
fn shorten_paths(title: &str) -> String {
    title
        .split(' ')
        .map(|word| {
            if !word.contains('/') || word.len() < 12 {
                return word.to_string();
            }
            let parts: Vec<&str> = word.split('/').collect();
            let last = parts.len() - 1;
            parts
                .iter()
                .enumerate()
                .map(|(i, part)| {
                    if i == last || part.is_empty() || *part == "~" {
                        (*part).to_string()
                    } else {
                        part.chars().next().map(String::from).unwrap_or_default()
                    }
                })
                .collect::<Vec<_>>()
                .join("/")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageConfig {
    /// Ask Claude Code to refresh its usage cache (`claude -p /usage`) when
    /// an account's numbers are older than this. 0 disables it, leaving the
    /// panel dependent on whatever Claude last wrote.
    pub refresh_minutes: u64,
}

impl Default for UsageConfig {
    fn default() -> Self {
        UsageConfig {
            refresh_minutes: 10,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateConfig {
    /// Ask GitHub once a day whether a newer release exists. This is the
    /// only network request Giverny makes; set false to make it zero.
    pub check: bool,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        UpdateConfig { check: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FontConfig {
    /// Preferred family; empty = auto-detect a monospace font.
    pub family: String,
    pub size: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    /// Built-in theme name: `monet-dark`, `monet-light`, `ink`.
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BehaviorConfig {
    /// Re-run `claude --resume` for restored tabs: `auto`, `prompt`, `off`.
    pub restore_claude: RestoreClaude,
    /// Desktop notifications when Claude needs you.
    pub notifications: bool,
    /// Scrollback lines kept per tab.
    pub scrollback_lines: usize,
    /// Extra `CLAUDE_CONFIG_DIR`s to treat as accounts.
    pub extra_profile_dirs: Vec<PathBuf>,
    /// Ask winit for the X11 backend on Linux (drag-and-drop works there;
    /// Wayland has no drop support in winit). Softer text under XWayland.
    pub prefer_x11: bool,
    /// Programs a restored tab may start again by itself. Anything not
    /// listed is remembered but never re-run — replaying an arbitrary last
    /// command could deploy, delete or push something.
    pub restore_apps: Vec<String>,
    /// Which shell a new tab opens on Windows. Ignored everywhere else,
    /// where `$SHELL` answers the question.
    pub windows_shell: WindowsShell,
}

/// The shell a Windows tab opens. `Auto` prefers WSL — where Claude Code and
/// unix tooling usually live — but only when a distribution is installed;
/// `wsl.exe` exists on every Windows whether or not there is anything behind
/// it, and a tab spawned into an empty one dies on an error message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowsShell {
    #[default]
    Auto,
    Wsl,
    Powershell,
    Cmd,
}

impl WindowsShell {
    pub fn as_str(self) -> &'static str {
        match self {
            WindowsShell::Auto => "auto",
            WindowsShell::Wsl => "wsl",
            WindowsShell::Powershell => "powershell",
            WindowsShell::Cmd => "cmd",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RestoreClaude {
    Auto,
    Prompt,
    Off,
}

impl Default for FontConfig {
    fn default() -> Self {
        FontConfig {
            family: String::new(),
            size: 13.0,
        }
    }
}

impl Default for ThemeConfig {
    fn default() -> Self {
        ThemeConfig {
            name: "monet-dark".into(),
        }
    }
}

impl Default for BehaviorConfig {
    fn default() -> Self {
        BehaviorConfig {
            prefer_x11: false,
            restore_claude: RestoreClaude::Auto,
            notifications: true,
            scrollback_lines: 10_000,
            extra_profile_dirs: Vec::new(),
            restore_apps: crate::procs::DEFAULT_RESTORE_APPS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            windows_shell: WindowsShell::Auto,
        }
    }
}

pub fn config_path(base: &Path) -> PathBuf {
    base.join("config.toml")
}

/// Dotted paths in `input` that `known` (the parsed config, re-serialized)
/// lacks. A key this build does not know — usually one a newer build wrote.
fn unknown_keys(input: &toml::Value, known: &toml::Value, prefix: &str, out: &mut Vec<String>) {
    let (Some(input), Some(known)) = (input.as_table(), known.as_table()) else {
        return;
    };
    for (key, value) in input {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match known.get(key) {
            Some(k) => unknown_keys(value, k, &path, out),
            None => out.push(path),
        }
    }
}

/// Parse config text. Keys this build does not know are dropped and returned
/// beside the config rather than failing the whole file; a value of the wrong
/// type is still an error.
pub fn parse(text: &str) -> Result<(Config, Vec<String>), toml::de::Error> {
    let cfg: Config = toml::from_str(text)?;
    let mut unknown = Vec::new();
    if let (Ok(input), Ok(known)) = (text.parse::<toml::Value>(), toml::Value::try_from(&cfg)) {
        unknown_keys(&input, &known, "", &mut unknown);
    }
    Ok((cfg, unknown))
}

fn read(path: &Path) -> Option<Result<Config, String>> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(match parse(&text) {
        Ok((cfg, unknown)) => {
            if !unknown.is_empty() {
                tracing::warn!("config.toml: ignoring unknown keys: {}", unknown.join(", "));
            }
            Ok(cfg)
        }
        Err(err) => Err(err.to_string()),
    })
}

/// Load the config, writing the commented template on first run. Unknown keys
/// are warned about and skipped; an invalid file is reported and ignored
/// rather than blocking startup.
pub fn load(base: &Path) -> Config {
    load_or(base, &Config::default())
}

/// Like [`load`], but a file that cannot be parsed yields `previous` instead
/// of defaults, so a hot-reload never resets running settings.
pub fn load_or(base: &Path, previous: &Config) -> Config {
    let path = config_path(base);
    match read(&path) {
        Some(Ok(cfg)) => cfg,
        Some(Err(err)) => {
            tracing::error!("config.toml ignored ({err}); keeping previous settings");
            previous.clone()
        }
        None => {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            // Generated from the settings table, so the file can never
            // document an option the app does not have.
            let _ = std::fs::write(&path, crate::settings::template());
            Config::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows names a console after the program that opened it, and ConPTY
    /// forwards that as the title.
    #[test]
    fn a_program_path_is_shown_as_the_program() {
        let cfg = TitlesConfig::default();
        assert_eq!(
            display_title(
                r"C:\WINDOWS\System32\WindowsPowerShell\v1.0\powershell.exe",
                &cfg
            ),
            "powershell"
        );
        assert_eq!(display_title(r"C:\WINDOWS\system32\cmd.exe", &cfg), "cmd");
        // A title someone chose is left alone, even when it names a program.
        assert_eq!(
            display_title("build C:\\tools\\make.exe", &cfg),
            "build C:\\tools\\make.exe"
        );
        assert_eq!(display_title("~/Dev/giverny", &cfg), "~/Dev/giverny");
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("giverny-cfg-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn first_run_writes_template_that_parses_to_defaults() {
        let dir = scratch("first");
        let cfg = load(&dir);
        assert!(config_path(&dir).exists(), "template written");
        assert_eq!(cfg.font.size, 13.0);
        assert_eq!(cfg.behavior.restore_claude, RestoreClaude::Auto);

        // The template on disk must itself be valid and match the defaults.
        let reparsed = load(&dir);
        assert_eq!(reparsed.theme.name, cfg.theme.name);
        assert_eq!(reparsed.behavior.scrollback_lines, 10_000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn partial_config_keeps_defaults_for_the_rest() {
        let dir = scratch("partial");
        std::fs::write(config_path(&dir), "[font]\nsize = 16.5\n").unwrap();
        let cfg = load(&dir);
        assert_eq!(cfg.font.size, 16.5);
        assert_eq!(cfg.theme.name, "monet-dark", "unspecified sections default");
        assert!(cfg.behavior.notifications);
        assert!(cfg.update.check);
        assert_eq!(cfg.usage.refresh_minutes, 10);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn host_prefix_is_stripped_only_when_it_really_is_one() {
        let cfg = TitlesConfig::default();
        // The actual shape oh-my-zsh sets, and the reason for the option.
        assert_eq!(
            display_title("yoz@yoz-framework:~/Dev/bobo", &cfg),
            "~/Dev/bobo"
        );
        assert_eq!(display_title("a@b: spaced", &cfg), "spaced");
        // Left alone: no colon, an @ that is not a prefix, and titles whose
        // prefix is not a plain name@host.
        for keep in [
            "✳ Claude Code",
            "btop",
            "ssh: user@host",
            "npm run build: watching",
            "git log --author=me@example.com",
            "~/Dev/bobo",
        ] {
            assert_eq!(
                display_title(keep, &cfg),
                keep,
                "{keep} should be untouched"
            );
        }
    }

    #[test]
    fn shortening_keeps_the_directory_you_are_in() {
        let cfg = TitlesConfig {
            strip_host_prefix: true,
            shorten_paths: true,
        };
        assert_eq!(
            display_title("yoz@host:~/Dev/claude_test/giverny", &cfg),
            "~/D/c/giverny"
        );
        // Short paths and non-paths are not worth mangling.
        assert_eq!(display_title("~/Dev", &cfg), "~/Dev");
        assert_eq!(display_title("btop", &cfg), "btop");
    }

    #[test]
    fn broken_config_falls_back_instead_of_failing() {
        let dir = scratch("broken");
        std::fs::write(config_path(&dir), "this is not toml {{{").unwrap();
        let cfg = load(&dir);
        assert_eq!(cfg.font.size, 13.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_keys_keep_the_known_ones() {
        let (cfg, unknown) = parse(
            "[claude]\nauto_mode = true\nfuture_key = 1\n[orchestrator]\nx = 2\n[font]\nsize = 20.0\n",
        )
        .unwrap();
        assert!(cfg.claude.auto_mode);
        assert_eq!(cfg.font.size, 20.0);
        assert_eq!(unknown, ["claude.future_key", "orchestrator"]);
    }

    #[test]
    fn reload_keeps_previous_on_invalid_value_but_not_on_unknown_key() {
        let dir = std::env::temp_dir().join(format!("giverny-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut prev = Config::default();
        prev.claude.auto_mode = true;
        prev.font.size = 31.0;
        // Unknown key: the known keys still apply.
        std::fs::write(config_path(&dir), "[font]\nsize = 20.0\nnew_key = true\n").unwrap();
        assert_eq!(load_or(&dir, &prev).font.size, 20.0);
        // Invalid value: running settings stay as they were.
        std::fs::write(config_path(&dir), "[font]\nsize = \"big\"\n").unwrap();
        let kept = load_or(&dir, &prev);
        assert_eq!(kept.font.size, 31.0);
        assert!(kept.claude.auto_mode);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
