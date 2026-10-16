use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Window {
    pub label: String,
    pub used_percent: f64,
    pub duration_minutes: Option<u64>,
    pub resets_at: Option<i64>,
}

/// Identity is supplied by account/read; credential files stay owned by Codex.
#[derive(Debug, Clone)]
pub struct AccountUsage {
    pub name: Option<String>,
    pub windows: Vec<Window>,
}

pub fn account_name(account: &Value) -> Option<String> {
    let email = account.get("email").and_then(Value::as_str)?;
    let local = email.split_once('@')?.0;
    let name: String = local
        .chars()
        .filter(|c| !c.is_control() && !c.is_whitespace())
        .take(48)
        .collect();
    (!name.is_empty()).then_some(name)
}

pub fn parse(result: &Value) -> anyhow::Result<Vec<Window>> {
    let mut out = Vec::new();
    let buckets: Vec<(&str, &Value)> = match result
        .get("rateLimitsByLimitId")
        .and_then(Value::as_object)
        .filter(|m| !m.is_empty())
    {
        Some(m) => m.iter().map(|(k, v)| (k.as_str(), v)).collect(),
        None => result
            .get("rateLimits")
            .filter(|v| v.is_object())
            .map(|v| vec![("codex", v)])
            .unwrap_or_default(),
    };
    if buckets.is_empty() {
        bail!("Codex returned no usage limits for this account")
    }
    for (id, bucket) in buckets {
        let name = bucket
            .get("limitName")
            .and_then(Value::as_str)
            .unwrap_or(id);
        for key in ["primary", "secondary"] {
            let Some(window) = bucket.get(key).filter(|v| !v.is_null()) else {
                continue;
            };
            let used = window
                .get("usedPercent")
                .and_then(Value::as_f64)
                .context("usage window has no valid percentage")?;
            if !used.is_finite() || !(0.0..=100.0).contains(&used) {
                bail!("invalid usage percentage")
            }
            let duration = window.get("windowDurationMins").and_then(Value::as_u64);
            let label = duration
                .map(|m| {
                    if m % 1440 == 0 {
                        format!("{}d", m / 1440)
                    } else if m % 60 == 0 {
                        format!("{}h", m / 60)
                    } else {
                        format!("{m}m")
                    }
                })
                .unwrap_or_else(|| key.into());
            out.push(Window {
                label: format!("{name} {label}"),
                used_percent: used,
                duration_minutes: duration,
                resets_at: window.get("resetsAt").and_then(Value::as_i64),
            });
        }
    }
    if out.is_empty() {
        bail!("No allowance windows available for this account")
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn names_use_account_metadata_with_an_anonymous_fallback() {
        assert_eq!(
            account_name(&json!({"email":"bobobot@example.test"})).as_deref(),
            Some("bobobot")
        );
        assert_eq!(account_name(&json!({"email":null})), None);
        assert_eq!(account_name(&json!({"email":""})), None);
        assert_eq!(
            account_name(&json!({"email":"\nname\t@example.test"})).as_deref(),
            Some("name")
        );
    }
    #[test]
    fn handles_catalog_without_duplicate_legacy_bucket() {
        let bucket = json!({"primary":{"usedPercent":25,"windowDurationMins":300,"resetsAt":100},"secondary":null});
        let v = json!({"rateLimits":bucket,"rateLimitsByLimitId":{"codex":bucket,"review":{"primary":{"usedPercent":0,"windowDurationMins":10080}}}});
        let windows = parse(&v).unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].label, "codex 5h");
        assert_eq!(windows[0].resets_at, Some(100));
        assert!(parse(&json!({})).is_err());
        assert!(parse(&json!({"rateLimits":{"primary":{"usedPercent":101}}})).is_err());
    }
}

/// Giverny's own last successful readings, never Codex credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub target: crate::Target,
    #[serde(default)]
    pub account_name: Option<String>,
    pub windows: Vec<Window>,
    pub sampled_at: u64,
}

pub fn load_cache(path: &std::path::Path) -> Vec<Snapshot> {
    use std::io::Read;
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut bytes = Vec::new();
    if file.take(1_048_577).read_to_end(&mut bytes).is_err() || bytes.len() > 1_048_576 {
        return Vec::new();
    }
    serde_json::from_slice::<Vec<Snapshot>>(&bytes)
        .unwrap_or_default()
        .into_iter()
        .filter(|s| {
            s.windows
                .iter()
                .all(|w| w.used_percent.is_finite() && (0.0..=100.0).contains(&w.used_percent))
        })
        .collect()
}

pub fn save_cache(path: &std::path::Path, snapshots: &[Snapshot]) -> anyhow::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?
    }
    let temp = path.with_extension("json.giverny-tmp");
    let mut file = std::fs::File::create(&temp)?;
    file.write_all(&serde_json::to_vec(snapshots)?)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(temp, path)?;
    Ok(())
}
