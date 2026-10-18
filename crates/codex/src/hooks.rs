//! Non-destructive installation of Giverny's observer hooks.
use anyhow::{Context, bail};
use serde_json::{Value, json};
use std::path::Path;

pub const EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "Stop",
    "Interrupt",
    "SessionEnd",
];
const DESCRIPTION: &str = "Giverny Codex observer";

fn ours(v: &Value) -> bool {
    v.get("description").and_then(Value::as_str) == Some(DESCRIPTION)
}

pub fn installed(path: &Path, command: &str) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let Ok(root) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    EVENTS.iter().all(|ev| {
        root["hooks"][ev].as_array().is_some_and(|items| {
            items
                .iter()
                .any(|v| ours(v) && v["hooks"][0]["command"].as_str() == Some(command))
        })
    })
}

pub fn set_installed(path: &Path, command: &str, enable: bool) -> anyhow::Result<()> {
    let original = match std::fs::read(path) {
        Ok(v) => Some(v),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    if !enable && original.is_none() {
        return Ok(());
    }
    let mut root: Value = original
        .as_deref()
        .map(serde_json::from_slice)
        .transpose()
        .context("won't modify invalid Codex hooks.json")?
        .unwrap_or_else(|| json!({}));
    let object = root
        .as_object_mut()
        .context("hooks.json must contain an object")?;
    let hooks = object
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("hooks must contain an object")?;
    for event in EVENTS {
        if !enable && !hooks.contains_key(*event) {
            continue;
        }
        let entries = hooks
            .entry(*event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .with_context(|| format!("hooks.{event} must be an array"))?;
        entries.retain(|v| !ours(v));
        if enable {
            entries.push(json!({"description": DESCRIPTION, "hooks": [{"type":"command", "command":command, "timeout":3}]}));
        }
    }
    hooks.retain(|_, v| v.as_array().is_none_or(|a| !a.is_empty()));
    let bytes = serde_json::to_vec_pretty(&root)?;
    if original
        .as_deref()
        .is_some_and(|v| serde_json::from_slice::<Value>(v).ok().as_ref() == Some(&root))
    {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?
    }
    let backup = path.with_extension("json.giverny-bak");
    if original.is_some() && !backup.exists() {
        std::fs::copy(path, backup).context("backing up hooks.json")?;
    }
    let temp = path.with_extension("json.giverny-tmp");
    if temp.exists() {
        bail!(
            "{} already exists; another hook edit may be in progress",
            temp.display()
        )
    }
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn install_and_remove_preserve_other_hooks_and_backup() {
        let dir = std::env::temp_dir().join(format!("giverny-codex-hooks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hooks.json");
        let original = json!({"unknown":true,"hooks":{"Stop":[{"hooks":[{"type":"command","command":"my-hook"}]}]}});
        std::fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
        set_installed(&path, "giverny relay --provider codex", true).unwrap();
        assert!(installed(&path, "giverny relay --provider codex"));
        let first = std::fs::read(&path).unwrap();
        set_installed(&path, "giverny relay --provider codex", true).unwrap();
        assert_eq!(first, std::fs::read(&path).unwrap());
        set_installed(&path, "giverny relay --provider codex", false).unwrap();
        let result: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(result, original);
        let backup: Value = serde_json::from_slice(
            &std::fs::read(path.with_extension("json.giverny-bak")).unwrap(),
        )
        .unwrap();
        assert_eq!(backup, original);
        std::fs::write(&path, b"invalid").unwrap();
        assert!(set_installed(&path, "giverny", true).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"invalid");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
