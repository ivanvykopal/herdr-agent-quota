use super::statusline::{settings_path, Adapter};
use crate::cache::{CacheStore, DEFAULT_WATCH_INTERVAL_SECONDS};
use crate::model::Provider;
use crate::presentation::pace_segment;
use crate::providers::claude::parse_statusline;
use anyhow::{Context, Result};
use serde_json::Value;
use std::io::{Read, Write};
use std::path::Path;

const CONFIG: Adapter = Adapter {
    label: "Claude",
    subcommand: "claude-statusline",
    backup_file: "claude-statusline.original.json",
};

pub fn check() -> Result<()> {
    let cache = CacheStore::from_env()?;
    let executable = std::env::current_exe().context("resolve plugin executable")?;
    CONFIG.check(
        &settings_path("CLAUDE_SETTINGS_FILE", ".claude/settings.json")?,
        cache.root(),
        &executable,
    )
}

pub fn apply() -> Result<()> {
    let cache = CacheStore::from_env()?;
    let executable = std::env::current_exe().context("resolve plugin executable")?;
    apply_at_with_refresh_interval(
        &settings_path("CLAUDE_SETTINGS_FILE", ".claude/settings.json")?,
        cache.root(),
        &executable,
        cache.watch_interval_seconds(),
    )
}

pub fn apply_with_refresh_interval(refresh_interval_seconds: u64) -> Result<()> {
    let cache = CacheStore::from_env()?;
    let executable = std::env::current_exe().context("resolve plugin executable")?;
    apply_at_with_refresh_interval(
        &settings_path("CLAUDE_SETTINGS_FILE", ".claude/settings.json")?,
        cache.root(),
        &executable,
        refresh_interval_seconds,
    )
}

pub fn uninstall() -> Result<()> {
    let cache = CacheStore::from_env()?;
    uninstall_at(
        &settings_path("CLAUDE_SETTINGS_FILE", ".claude/settings.json")?,
        cache.root(),
    )
}

pub fn apply_at(settings: &Path, state: &Path, executable: &Path) -> Result<()> {
    apply_at_with_refresh_interval(settings, state, executable, DEFAULT_WATCH_INTERVAL_SECONDS)
}

pub fn apply_at_with_refresh_interval(
    settings: &Path,
    state: &Path,
    executable: &Path,
    refresh_interval_seconds: u64,
) -> Result<()> {
    CONFIG.apply_with_refresh_interval(settings, state, executable, Some(refresh_interval_seconds))
}

pub fn uninstall_at(settings: &Path, state: &Path) -> Result<()> {
    CONFIG.uninstall(settings, state)
}

pub fn run_statusline_hook() -> Result<()> {
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input)?;
    let mut pace = None;
    if let Ok(mut value) = serde_json::from_slice::<Value>(&input) {
        let now_unix = CacheStore::now_unix();
        // A gateway-routed session reports the routed model in its payload
        // (`model.id` is the gateway's id, not a `claude-*` one). Its
        // `rate_limits` describe the gateway, not the Anthropic account, so
        // they must not enter the cache or the pace segment.
        let gateway_model = gateway_model_id(&value).map(str::to_string);
        if gateway_model.is_some() {
            if let Some(object) = value.as_object_mut() {
                object.remove("rate_limits");
                object.remove("rateLimits");
            }
        }
        if let Ok(mut snapshot) = parse_statusline(&value, now_unix) {
            pace = pace_segment(&snapshot.windows, now_unix);
            // `display_name` is the logical Anthropic selection ("Opus 4.8"),
            // not what the gateway served. Record the served id and the
            // gateway marker here, so the hook and the transcript enrichment
            // agree instead of overwriting each other on every tick.
            if let Some(model) = gateway_model {
                snapshot.model = Some(model);
                if let Some(session_id) = value.get("session_id").and_then(Value::as_str) {
                    snapshot
                        .session_gateway_routed
                        .insert(session_id.to_string(), ());
                }
            }
            if let Ok(cache) = CacheStore::from_env() {
                let changed = observation_changed(&cache, &snapshot);
                let _ = cache.save_statusline_observation(Provider::Claude, snapshot, &value);
                if changed {
                    publish_in_background();
                }
            }
        }
    }
    let cache = CacheStore::from_env()?;
    let Some(output) = CONFIG.run_previous(cache.root(), &input)? else {
        if let Some(pace) = pace {
            println!("{pace}");
        }
        return Ok(());
    };
    if output.timed_out {
        return Ok(());
    }
    let stdout = if output.exit_code == Some(0) {
        append_pace(output.stdout, pace.as_deref())
    } else {
        output.stdout
    };
    std::io::stdout().write_all(&stdout)?;
    std::io::stdout().flush()?;
    if output.exit_code != Some(0) {
        std::process::exit(output.exit_code.unwrap_or(1));
    }
    Ok(())
}

/// Whether this tick changes what the sidebar shows (quota or model).
///
/// The statusLine often ticks after Herdr's idle event, when no refresh is
/// coming; without a publish here the pane would keep stale limits until the
/// next focus or turn. Unchanged ticks stay publish-free.
fn observation_changed(cache: &CacheStore, snapshot: &crate::model::ProviderSnapshot) -> bool {
    let Ok(Some(previous)) = cache.load_statusline_observation(Provider::Claude) else {
        return true;
    };
    previous.snapshot.windows != snapshot.windows || previous.snapshot.model != snapshot.model
}

/// Publish Claude panes from a detached process so the hook returns at once.
/// Only inside a Herdr pane: elsewhere there is no sidebar to update.
fn publish_in_background() {
    if std::env::var_os("HERDR_PANE_ID").is_none() {
        return;
    }
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    let mut command = std::process::Command::new(executable);
    command
        .args(["refresh", "--provider", "claude"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    crate::platform::detach(&mut command);
    let _ = command.spawn();
}

/// The payload's model id when it names a non-Anthropic (gateway) model.
///
/// Claude Code puts the routed model id in `model.id` — `claude-*` for
/// direct serving, the gateway's own id otherwise. A missing or empty id is
/// not evidence of anything and stays treated as direct.
fn gateway_model_id(value: &Value) -> Option<&str> {
    value
        .get("model")
        .and_then(|model| model.get("id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .filter(|id| !crate::providers::claude::is_anthropic_model_id(id))
}

/// Add the pace to the end of the wrapped command's last line so the status
/// line keeps whatever layout the user's own script produced.
fn append_pace(mut stdout: Vec<u8>, pace: Option<&str>) -> Vec<u8> {
    let Some(pace) = pace else {
        return stdout;
    };
    let newline = stdout.ends_with(b"\n");
    while stdout.last() == Some(&b'\n') {
        stdout.pop();
    }
    if !stdout.is_empty() {
        stdout.push(b' ');
    }
    stdout.extend_from_slice(pace.as_bytes());
    if newline {
        stdout.push(b'\n');
    }
    stdout
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_gateway_model_id_marks_the_payload_gateway_routed() {
        assert_eq!(
            gateway_model_id(&json!({
                "model": {"id": "glm-5.3", "display_name": "Opus 4.8"}
            })),
            Some("glm-5.3")
        );
        assert_eq!(
            gateway_model_id(&json!({
                "model": {"id": "claude-opus-4-8", "display_name": "Opus 4.8"}
            })),
            None
        );
    }

    #[test]
    fn a_missing_or_malformed_model_id_is_not_gateway_evidence() {
        assert_eq!(gateway_model_id(&json!({"model": {"display_name": "Opus"}})), None);
        assert_eq!(gateway_model_id(&json!({"model": {"id": "  "}})), None);
        assert_eq!(gateway_model_id(&json!({})), None);
    }

    #[test]
    fn pace_joins_the_last_status_line_and_keeps_the_trailing_newline() {
        assert_eq!(
            append_pace(b"a\nb\n".to_vec(), Some("⏱ 5h =")),
            "a\nb ⏱ 5h =\n".as_bytes()
        );
        assert_eq!(append_pace(b"a".to_vec(), Some("x")), b"a x");
        assert_eq!(append_pace(b"".to_vec(), Some("x")), b"x");
        assert_eq!(append_pace(b"a\n".to_vec(), None), b"a\n");
    }
}
