//! Bounded, synchronous stdio JSON-RPC, called only from background workers.
use crate::Target;
use anyhow::{Context, bail};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

pub struct Client {
    child: Child,
    input: ChildStdin,
    messages: Receiver<anyhow::Result<Value>>,
    next_id: u64,
    deadline: Instant,
    cancel: Arc<AtomicBool>,
}

impl Client {
    pub fn connect(target: &Target, cancel: Arc<AtomicBool>) -> anyhow::Result<Self> {
        Self::spawn(target, &["app-server", "--listen", "stdio://"], cancel)
    }

    fn spawn(target: &Target, args: &[&str], cancel: Arc<AtomicBool>) -> anyhow::Result<Self> {
        let mut cmd = target.command(args);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
        }
        let mut child = cmd
            .spawn()
            .context("could not start Codex; check executable and PATH")?;
        let input = child.stdin.take().context("Codex stdin unavailable")?;
        let output = child.stdout.take().context("Codex stdout unavailable")?;
        let (tx, rx) = mpsc::sync_channel(16);
        let spawned = std::thread::Builder::new()
            .name("codex rpc reader".into())
            .spawn(move || {
                let mut reader = BufReader::new(output);
                loop {
                    let mut bytes = Vec::new();
                    let read = reader
                        .by_ref()
                        .take(1_048_577)
                        .read_until(b'\n', &mut bytes);
                    let value = match read {
                        Ok(0) => break,
                        Ok(_) if bytes.len() > 1_048_576 => {
                            Err(anyhow::anyhow!("Codex response exceeds 1 MiB"))
                        }
                        Ok(_) => serde_json::from_slice(&bytes)
                            .context("invalid Codex JSON-RPC response"),
                        Err(e) => Err(e.into()),
                    };
                    let failed = value.is_err();
                    if tx.send(value).is_err() || failed {
                        break;
                    }
                }
            });
        if let Err(e) = spawned {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e.into());
        }
        let mut client = Self {
            child,
            input,
            messages: rx,
            next_id: 1,
            deadline: Instant::now() + Duration::from_secs(15),
            cancel,
        };
        client.request("initialize", json!({"clientInfo":{"name":"giverny","title":"Giverny","version":env!("CARGO_PKG_VERSION")}}))?;
        client.send(json!({"method":"initialized"}))?;
        Ok(client)
    }

    fn send(&mut self, message: Value) -> anyhow::Result<()> {
        serde_json::to_writer(&mut self.input, &message)?;
        self.input.write_all(b"\n")?;
        self.input.flush()?;
        Ok(())
    }

    pub fn request(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        if self.cancel.load(Ordering::Relaxed) {
            bail!("Codex request cancelled")
        }
        self.send(json!({"id":id,"method":method,"params":params}))?;
        loop {
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if self.cancel.load(Ordering::Relaxed) {
                bail!("Codex request cancelled")
            }
            if remaining.is_zero() {
                bail!("Codex request timed out")
            }
            let message = match self
                .messages
                .recv_timeout(remaining.min(Duration::from_millis(100)))
            {
                Ok(v) => v?,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => bail!("Codex helper exited before responding"),
            };
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(error) = message.get("error") {
                    bail!(
                        "Codex {method}: {}",
                        error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("request failed")
                    );
                }
                return message
                    .get("result")
                    .cloned()
                    .context("Codex response has no result");
            }
            // Observers do not answer approvals or service-owned token requests.
            // Explicitly reject unsolicited server requests instead of hanging them.
            if message.get("method").is_some()
                && let Some(id) = message.get("id")
            {
                self.send(json!({"id":id,"error":{"code":-32601,"message":"Giverny is a read-only observer"}}))?;
            }
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn fetch_usage(
    target: &Target,
    cancel: Arc<AtomicBool>,
) -> anyhow::Result<crate::usage::AccountUsage> {
    let mut client = Client::connect(target, cancel.clone())?;
    let account = client.request("account/read", json!({"refreshToken":false}))?;
    match account["account"]["type"].as_str() {
        Some("apiKey" | "amazonBedrock") => {
            bail!("Subscription limits unavailable for this authentication mode")
        }
        None if account["requiresOpenaiAuth"] == true => {
            bail!("Sign in using codex login, then refresh usage")
        }
        _ => {}
    }
    Ok(crate::usage::AccountUsage {
        name: crate::usage::account_name(&account["account"]),
        windows: crate::usage::parse(&client.request("account/rateLimits/read", json!({}))?)?,
    })
}

/// Reading metadata never resumes a thread or starts a model turn.
pub fn inspect_session(
    target: &Target,
    id: &str,
    cancel: Arc<AtomicBool>,
) -> anyhow::Result<Value> {
    if !crate::valid_session_id(id) {
        bail!("Invalid Codex session id")
    }
    let mut client = Client::connect(target, cancel)?;
    // Loaded threads may simply be retained by the shared daemon after a
    // terminal disconnects. The regular CLI resume reconnects to them; a
    // loaded-thread inventory is not evidence of another live frontend.
    client.request("thread/read", json!({"threadId":id, "includeTurns":false}))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU64;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        path: PathBuf,
        target: Target,
    }
    impl Fixture {
        fn new(reply: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "giverny-codex-rpc-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            let executable = path.join("codex-fixture");
            std::fs::write(&executable,format!(r#"#!/bin/sh
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$CODEX_HOME/requests"
  case "$line" in
    *'"method":"initialize"'*) printf '%s\n' '{{"id":1,"result":{{}}}}' ;;
    *'"method":"account/read"'*) {reply} ;;
    *'"method":"account/rateLimits/read"'*) printf '%s\n' '{{"method":"account/rateLimits/updated","params":{{}}}}' '{{"id":3,"result":{{"rateLimits":{{"primary":{{"usedPercent":42,"windowDurationMins":300}}}}}}}}' ;;
  esac
done
"#)).unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self {
                target: Target {
                    executable: executable.to_string_lossy().into_owned(),
                    home: path.clone(),
                    wsl_distro: None,
                },
                path,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
    fn token() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }
    #[test]
    fn usage_handshake_handles_notifications_and_never_starts_a_turn() {
        let fixture = Fixture::new(
            "printf '%s\\n' '{\"id\":2,\"result\":{\"account\":{\"type\":\"chatgpt\"}}}'",
        );
        let windows = fetch_usage(&fixture.target, token()).unwrap();
        assert_eq!(windows.windows[0].used_percent, 42.0);
        let requests = std::fs::read_to_string(fixture.path.join("requests")).unwrap();
        assert!(requests.contains("initialized"));
        assert!(requests.contains("account/rateLimits/read"));
        for forbidden in [
            "turn/start",
            "thread/resume",
            "account/login",
            "account/logout",
            "apiKey",
        ] {
            assert!(!requests.contains(forbidden));
        }
    }
    #[test]
    fn malformed_messages_crashes_and_unsupported_auth_are_errors() {
        for reply in [
            "printf '%s\\n' 'not json'",
            "exit 3",
            "printf '%s\\n' '{\"id\":2,\"result\":{\"account\":{\"type\":\"apiKey\"}}}'",
        ] {
            let fixture = Fixture::new(reply);
            assert!(fetch_usage(&fixture.target, token()).is_err());
        }
    }
    #[test]
    fn timeouts_and_cancellation_release_the_owned_helper() {
        let fixture = Fixture::new(":");
        let cancel = token();
        let mut client = Client::connect(&fixture.target, cancel.clone()).unwrap();
        client.deadline = Instant::now() + Duration::from_millis(30);
        assert!(
            client
                .request("account/read", json!({}))
                .unwrap_err()
                .to_string()
                .contains("timed out")
        );
        drop(client);
        let mut client = Client::connect(&fixture.target, cancel.clone()).unwrap();
        cancel.store(true, Ordering::Relaxed);
        assert!(
            client
                .request("account/read", json!({}))
                .unwrap_err()
                .to_string()
                .contains("cancelled")
        );
    }
    #[test]
    fn restore_reads_metadata_without_requiring_a_shared_daemon() {
        let fixture = Fixture::new(":");
        let sid = "01a144ed-3193-79d3-9569-4b9e28f3d9e4";
        let executable = PathBuf::from(&fixture.target.executable);
        let script = std::fs::read_to_string(&executable).unwrap();
        let handlers = format!(
            r#"*'"method":"thread/read"'*) printf '%s\n' '{{"id":2,"result":{{"thread":{{"id":"{sid}","cwd":"/work","status":{{"type":"idle"}}}}}}}}' ;;
    "#
        );
        std::fs::write(
            &executable,
            script.replace(
                "*'\"method\":\"account/read\"'*)",
                &(handlers + "*'\"method\":\"account/read\"'*)"),
            ),
        )
        .unwrap();
        let metadata = inspect_session(&fixture.target, sid, token()).unwrap();
        assert_eq!(metadata["thread"]["cwd"], "/work");
        assert_eq!(metadata["thread"]["status"]["type"], "idle");
        let requests = std::fs::read_to_string(fixture.path.join("requests")).unwrap();
        assert!(requests.contains("\"includeTurns\":false"));
        assert!(!requests.contains("thread/loaded/list"));
        assert!(!requests.contains("thread/resume"));
    }
}
