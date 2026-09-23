use crate::cache::CacheStore;
use crate::model::{ContextUsage, Provider, ProviderSnapshot, ResetAt, UsageWindow, WindowKind};
use crate::providers::statusline::{parse_context, parse_model};
use crate::providers::ProviderError;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

/// Opaque Claude profile identity used to share quota across sessions.
///
/// Claude Code's `CLAUDE_CONFIG_DIR` is the profile root for credentials and
/// session history. The cache stores only a SHA-256 hex digest of the
/// normalized path, never the path itself.
pub fn quota_scope_id(
    claude_config_dir: Option<&OsStr>,
    home: Option<&OsStr>,
    current_dir: Option<&Path>,
) -> Option<String> {
    let config_dir = claude_config_dir.filter(|value| !value.is_empty());
    let home_path = home.filter(|value| !value.is_empty()).map(PathBuf::from);
    let raw = match config_dir {
        Some(dir) => PathBuf::from(dir),
        None => home_path.as_ref()?.join(".claude"),
    };
    let normalized = normalize_profile_root(&raw, home_path.as_deref(), current_dir);
    Some(hash_profile_root(&normalized))
}

fn normalize_profile_root(path: &Path, home: Option<&Path>, current_dir: Option<&Path>) -> PathBuf {
    let expanded = expand_tilde(path, home);
    let absolute = if expanded.is_absolute() {
        expanded
    } else if let Some(cwd) = current_dir {
        cwd.join(expanded)
    } else {
        expanded
    };
    normalize_components(&absolute)
}

fn expand_tilde(path: &Path, home: Option<&Path>) -> PathBuf {
    let mut components = path.components();
    match (components.next(), home) {
        (Some(Component::Normal(first)), Some(home)) if first == "~" => {
            let rest = components.as_path();
            if rest.as_os_str().is_empty() {
                home.to_path_buf()
            } else {
                home.join(rest)
            }
        }
        _ => path.to_path_buf(),
    }
}

fn normalize_components(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => out.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::Normal(name) => out.push(name),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

fn hash_profile_root(path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"claude-quota-scope\0");
    hasher.update(path.as_os_str().as_encoded_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn parse_statusline(
    value: &Value,
    fetched_at_unix: u64,
) -> std::result::Result<ProviderSnapshot, ProviderError> {
    let mut context = parse_context(
        value
            .get("context_window")
            .or_else(|| value.get("contextWindow")),
    )
    .unwrap_or(None);
    apply_prompt_cache(
        &mut context,
        value
            .get("prompt_cache")
            .or_else(|| value.get("promptCache")),
    );
    let model = parse_model(value);
    let Some(limits) = value.get("rate_limits") else {
        return Ok(
            ProviderSnapshot::new(Provider::Claude, vec![], fetched_at_unix)
                .with_model(model)
                .with_context(context),
        );
    };
    let mut windows = Vec::new();
    if let Some(window) = parse_window(limits.get("five_hour"), WindowKind::FiveHour)? {
        windows.push(window);
    }
    if let Some(window) = parse_window(limits.get("seven_day"), WindowKind::Weekly)? {
        windows.push(window);
    }
    if windows.is_empty() {
        return Ok(
            ProviderSnapshot::new(Provider::Claude, vec![], fetched_at_unix)
                .with_model(model)
                .with_context(context),
        );
    }
    Ok(
        ProviderSnapshot::new(Provider::Claude, windows, fetched_at_unix)
            .with_model(model)
            .with_context(context),
    )
}

fn parse_window(
    value: Option<&Value>,
    kind: WindowKind,
) -> std::result::Result<Option<UsageWindow>, ProviderError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let used = value
        .get("used_percentage")
        .and_then(Value::as_f64)
        .ok_or_else(|| {
            ProviderError::UnsupportedResponse(format!("missing {} usage", kind.label()))
        })?;
    let reset = value.get("resets_at").and_then(|value| {
        value
            .as_u64()
            .map(ResetAt::from_unix_seconds)
            .or_else(|| value.as_str().and_then(ResetAt::parse))
    });
    UsageWindow::new(kind, used, reset)
        .map(Some)
        .map_err(|error| ProviderError::UnsupportedResponse(error.to_string()))
}

/// Claude Code v2.1.251+ reports the live prefix expiry on statusLine stdin.
/// Cold or missing expiry clears any previous countdown instead of guessing
/// from a transcript bucket.
pub(crate) fn apply_prompt_cache(context: &mut Option<ContextUsage>, prompt_cache: Option<&Value>) {
    let Some(prompt_cache) = prompt_cache.filter(|value| !value.is_null()) else {
        return;
    };
    let Some(object) = prompt_cache.as_object() else {
        return;
    };
    let Some(cache) = context.as_mut().and_then(|context| context.cache.as_mut()) else {
        return;
    };
    let warm = object.get("warm").and_then(Value::as_bool) != Some(false);
    let expires_at = object.get("expires_at").and_then(parse_expires_at);
    match (warm, expires_at) {
        (true, Some(expires_at)) => {
            cache.expires_at_unix = Some(expires_at);
            cache.ttl_seconds = object
                .get("ttl")
                .and_then(Value::as_str)
                .and_then(parse_prompt_cache_ttl);
            if let Some(ttl_seconds) = cache.ttl_seconds {
                cache.last_activity_unix = Some(expires_at.saturating_sub(ttl_seconds));
            }
        }
        _ => {
            cache.expires_at_unix = Some(0);
            cache.ttl_seconds = None;
            cache.last_activity_unix = None;
        }
    }
}

fn parse_prompt_cache_ttl(value: &str) -> Option<u64> {
    match value.trim() {
        "5m" => Some(5 * 60),
        "1h" => Some(60 * 60),
        _ => None,
    }
}

fn parse_expires_at(value: &Value) -> Option<u64> {
    if value.is_null() {
        return None;
    }
    value
        .as_u64()
        .or_else(|| {
            let number = value.as_f64()?;
            (number.is_finite() && number >= 0.0).then_some(number.round() as u64)
        })
        .or_else(|| {
            value
                .as_str()
                .and_then(ResetAt::parse)
                .map(ResetAt::unix_seconds)
        })
}

pub fn run_statusline(input: &[u8]) -> std::result::Result<ProviderSnapshot, ProviderError> {
    let value: Value = serde_json::from_slice(input).map_err(|_| {
        ProviderError::UnsupportedResponse("statusLine input is not JSON".to_string())
    })?;
    parse_statusline(&value, CacheStore::now_unix())
}

/// The model the gateway actually served for a session, from its transcript.
///
/// A gateway-routed Claude Code still reports the logical selection in
/// statusLine `model.display_name` ("Opus 4.8"), while the transcript records
/// the served model on every assistant turn (`message.model`: "glm-5.3").
/// The transcript is the local ground truth, so the last assistant turn's
/// model wins over the declared name.
///
/// Reads only the tail of the file: the newest entry carries the current
/// model, so scanning the whole transcript is wasted work.
pub(crate) fn transcript_model(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    let start = length.saturating_sub(TRANSCRIPT_TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut tail = Vec::new();
    file.take(TRANSCRIPT_TAIL_BYTES)
        .read_to_end(&mut tail)
        .ok()?;
    let mut window = tail.as_slice();
    if start > 0 {
        // Drop a partial line cut by the tail boundary.
        if let Some(index) = window.iter().position(|byte| *byte == b'\n') {
            window = &window[index + 1..];
        } else {
            window = &[];
        }
    }
    let mut model = None;
    for line in window.split(|byte| *byte == b'\n') {
        let Ok(entry) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        // The `model` attachment lands with the first prompt, before any
        // assistant turn; the assistant entries confirm or update it after.
        if entry.get("type").and_then(Value::as_str) == Some("attachment")
            && entry.pointer("/attachment/type").and_then(Value::as_str) == Some("model")
        {
            if let Some(declared) = entry
                .pointer("/attachment/identity/modelId")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|model| !model.is_empty())
            {
                model = Some(declared.to_string());
            }
        }
        if entry.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        if let Some(served) = entry
            .pointer("/message/model")
            .and_then(Value::as_str)
            .map(str::trim)
            // Claude Code writes `<synthetic>` for turns it fabricates locally
            // (interrupts, API errors). It names no served model; taking it
            // would blank the label and mark a direct session gateway-routed.
            .filter(|model| !model.is_empty() && *model != "<synthetic>")
        {
            model = Some(served.to_string());
        }
    }
    model
}

const TRANSCRIPT_TAIL_BYTES: u64 = 256 * 1024;

/// Whether a served model id was served by Anthropic itself.
///
/// Anthropic model ids start with `claude-` (e.g. `claude-opus-4-8`,
/// `claude-glm-alias` is not a thing: gateways keep their own ids). A gateway
/// that aliases its models as `claude-*` is indistinguishable from direct
/// serving, and that is acceptable — its quota is genuinely unknown either
/// way, and showing the profile's windows is the long-standing behavior.
pub(crate) fn is_anthropic_model_id(model: &str) -> bool {
    model.trim().to_ascii_lowercase().starts_with("claude-")
}

/// Locate the transcript for a session by id under the profile root.
///
/// Sessions live in `<config dir>/projects/<munged cwd>/<session-id>.jsonl`.
/// The munged cwd cannot be reconstructed from the pane data (its casing is
/// not stable), so every project directory is scanned for a file named after
/// the session. A session id containing a path separator would escape the
/// scan, so it is rejected up front.
pub(crate) fn transcript_for_session(
    claude_config_dir: Option<&OsStr>,
    session_id: &str,
) -> Option<PathBuf> {
    if session_id.contains(['/', '\\']) {
        return None;
    }
    let config_dir = match claude_config_dir.filter(|value| !value.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => crate::platform::home_dir()?.join(".claude"),
    };
    let projects = config_dir.join("projects");
    let filename = format!("{session_id}.jsonl");
    let candidates = std::fs::read_dir(&projects).ok()?;
    for entry in candidates.flatten() {
        let path = entry.path().join(&filename);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::ffi::OsStr;
    use std::path::Path;

    #[test]
    fn parses_claude_five_hour_and_weekly_limits() {
        let value = json!({
            "rate_limits": {
                "five_hour": {"used_percentage": 58.0, "resets_at": 1786795200},
                "seven_day": {"used_percentage": 27.0, "resets_at": 1787400000}
            }
        });
        let snapshot = parse_statusline(&value, 1).unwrap();
        assert_eq!(
            snapshot.window(WindowKind::FiveHour).unwrap().resets_at,
            Some(ResetAt::from_unix_seconds(1_786_795_200))
        );
    }

    #[test]
    fn parses_optional_context_window_usage() {
        let value = json!({
            "context_window": {
                "used_percentage": 23.5,
                "remaining_percentage": 76.5,
                "current_usage": {
                    "input_tokens": 100,
                    "cache_read_input_tokens": 800,
                    "cache_creation_input_tokens": 100
                }
            },
            "rate_limits": {
                "five_hour": {"used_percentage": 58.0}
            }
        });
        let snapshot = parse_statusline(&value, 1).unwrap();
        assert_eq!(
            snapshot
                .context
                .as_ref()
                .map(|context| context.used_percent),
            Some(23.5)
        );
        let cache = snapshot.context.as_ref().unwrap().cache.as_ref().unwrap();
        assert_eq!(cache.read_tokens, 800);
        assert_eq!(cache.creation_tokens, 100);
        assert_eq!(cache.hit_percent, 80.0);
    }

    #[test]
    fn parses_the_human_readable_active_model_name() {
        let value = json!({
            "model": {"id": "claude-sonnet-4-20250514", "display_name": "Sonnet"},
            "rate_limits": {"five_hour": {"used_percentage": 1.0}}
        });
        let snapshot = parse_statusline(&value, 1).unwrap();
        assert_eq!(snapshot.model.as_deref(), Some("Sonnet"));
    }

    #[test]
    fn reads_prompt_cache_expiry_from_statusline() {
        let value = json!({
            "context_window": {
                "used_percentage": 23.5,
                "current_usage": {
                    "input_tokens": 10,
                    "cache_read_input_tokens": 80,
                    "cache_creation_input_tokens": 10
                }
            },
            "prompt_cache": {
                "warm": true,
                "ttl": "1h",
                "expires_at": 1_787_396_400
            },
            "rate_limits": {
                "five_hour": {"used_percentage": 58.0}
            }
        });
        let snapshot = parse_statusline(&value, 1).unwrap();
        let cache = snapshot.context.unwrap().cache.unwrap();
        assert_eq!(cache.expires_at_unix, Some(1_787_396_400));
        assert_eq!(cache.ttl_seconds, Some(60 * 60));
        assert_eq!(cache.last_activity_unix, Some(1_787_392_800));
        assert_eq!(cache.remaining_ttl_seconds(1_787_392_800), Some(3_600));
    }

    #[test]
    fn cold_prompt_cache_clears_ttl() {
        let value = json!({
            "context_window": {
                "used_percentage": 23.5,
                "current_usage": {
                    "input_tokens": 10,
                    "cache_read_input_tokens": 80,
                    "cache_creation_input_tokens": 0
                }
            },
            "prompt_cache": {
                "warm": false,
                "ttl": "1h",
                "expires_at": null
            },
            "rate_limits": {
                "five_hour": {"used_percentage": 58.0}
            }
        });
        let snapshot = parse_statusline(&value, 1).unwrap();
        let cache = snapshot.context.unwrap().cache.unwrap();
        assert_eq!(cache.expires_at_unix, Some(0));
        assert!(cache.ttl_seconds.is_none());
        assert_eq!(cache.remaining_ttl_seconds(1_787_392_800), Some(0));
    }

    #[test]
    fn ignores_transcript_buckets_when_prompt_cache_is_absent() {
        let value = json!({
            "transcript_path": "/tmp/unused.jsonl",
            "context_window": {
                "used_percentage": 23.5,
                "current_usage": {
                    "input_tokens": 10,
                    "cache_read_input_tokens": 80,
                    "cache_creation_input_tokens": 10
                }
            },
            "rate_limits": {
                "five_hour": {"used_percentage": 58.0}
            }
        });
        let snapshot = parse_statusline(&value, 1).unwrap();
        let cache = snapshot.context.unwrap().cache.unwrap();
        assert!(cache.expires_at_unix.is_none());
        assert!(cache.ttl_seconds.is_none());
        assert!(cache.last_activity_unix.is_none());
    }

    #[test]
    fn parses_rfc3339_reset_emitted_by_claude_statusline() {
        let value = json!({
            "rate_limits": {
                "five_hour": {
                    "used_percentage": 57.0,
                    "resets_at": "2026-08-15T12:00:00Z"
                }
            }
        });
        let snapshot = parse_statusline(&value, 1).unwrap();
        assert_eq!(
            snapshot.window(WindowKind::FiveHour).unwrap().resets_at,
            Some(ResetAt::from_unix_seconds(1_786_795_200))
        );
    }

    #[test]
    fn allows_a_missing_claude_window() {
        let value = json!({"rate_limits": {"five_hour": null}});
        assert!(parse_statusline(&value, 1).unwrap().windows.is_empty());
        let value = json!({
            "rate_limits": {"seven_day": {"used_percentage": 25.0}}
        });
        assert_eq!(parse_statusline(&value, 1).unwrap().windows.len(), 1);
    }

    #[test]
    fn accepts_a_payload_without_rate_limits_to_clear_a_stale_quota() {
        let value = json!({"context_window": {"used_percentage": 43.0}});
        let snapshot = parse_statusline(&value, 1).unwrap();
        assert!(snapshot.windows.is_empty());
        assert_eq!(
            snapshot
                .context
                .as_ref()
                .map(|context| context.used_percent),
            Some(43.0)
        );
    }

    #[test]
    fn rejects_non_json_statusline_input() {
        assert!(run_statusline(b"not-json").is_err());
    }

    #[test]
    fn transcript_model_reports_the_last_served_model() {
        let transcript = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            transcript.path(),
            concat!(
                r#"{"type":"user","message":{"role":"user"},"content":"hi"}"#,
                "\n",
                r#"{"type":"assistant","message":{"model":"glm-5.3","role":"assistant"}}"#,
                "\n",
                r#"{"type":"user","message":{"role":"user"}}"#,
                "\n",
                r#"{"type":"assistant","message":{"model":"kimi-k3","role":"assistant"}}"#,
                "\n",
            ),
        )
        .unwrap();
        assert_eq!(
            transcript_model(transcript.path()).as_deref(),
            Some("kimi-k3")
        );
    }

    #[test]
    fn transcript_model_ignores_non_assistant_models() {
        let transcript = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            transcript.path(),
            concat!(
                r#"{"type":"assistant","message":{"model":"glm-5.3"}}"#,
                "\n",
                r#"{"type":"summary","model":"not-a-model"}"#,
                "\n",
            ),
        )
        .unwrap();
        assert_eq!(
            transcript_model(transcript.path()).as_deref(),
            Some("glm-5.3")
        );
    }

    #[test]
    fn an_assistant_turn_overrides_the_declared_attachment() {
        let transcript = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            transcript.path(),
            concat!(
                r#"{"type":"attachment","attachment":{"type":"model","identity":{"modelId":"kimi-k3"}}}"#,
                "\n",
                r#"{"type":"assistant","message":{"model":"glm-5.3"}}"#, "\n",
            ),
        )
        .unwrap();
        assert_eq!(
            transcript_model(transcript.path()).as_deref(),
            Some("glm-5.3")
        );
    }

    #[test]
    fn transcript_model_ignores_synthetic_turns() {
        let transcript = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            transcript.path(),
            concat!(
                r#"{"type":"assistant","message":{"model":"kimi-k3"}}"#,
                "\n",
                r#"{"type":"assistant","message":{"model":"<synthetic>"}}"#,
                "\n",
            ),
        )
        .unwrap();
        assert_eq!(
            transcript_model(transcript.path()).as_deref(),
            Some("kimi-k3")
        );
    }

    #[test]
    fn transcript_for_session_scans_project_directories() {
        let home = tempfile::tempdir().unwrap();
        let config = home.path().join(".claude");
        let project = config.join("projects").join("C--Users-me-proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("session-1.jsonl"),
            r#"{"type":"assistant","message":{"model":"glm-5.3"}}"#,
        )
        .unwrap();
        let path = transcript_for_session(Some(config.as_os_str()), "session-1");
        assert_eq!(
            path.as_deref(),
            Some(project.join("session-1.jsonl").as_path())
        );
    }

    #[test]
    fn transcript_for_session_rejects_path_shaped_ids() {
        assert!(transcript_for_session(None, "../escape").is_none());
        assert!(transcript_for_session(None, r"..\escape").is_none());
    }

    #[test]
    fn gateway_models_are_distinguished_from_anthropic_ids() {
        assert!(is_anthropic_model_id("claude-opus-4-8"));
        assert!(is_anthropic_model_id("Claude-Sonnet-4-5"));
        assert!(!is_anthropic_model_id("glm-5.3"));
        assert!(!is_anthropic_model_id("kimi-k3"));
        assert!(!is_anthropic_model_id(""));
    }

    #[test]
    fn quota_scope_is_stable_for_the_same_profile_root() {
        let home = OsStr::new("/Users/me");
        let unset = quota_scope_id(None, Some(home), None).unwrap();
        let explicit =
            quota_scope_id(Some(OsStr::new("/Users/me/.claude")), Some(home), None).unwrap();
        let trailing =
            quota_scope_id(Some(OsStr::new("/Users/me/.claude/")), Some(home), None).unwrap();
        assert_eq!(unset, explicit);
        assert_eq!(unset, trailing);
        assert_eq!(unset.len(), 64);
        assert!(unset.chars().all(|ch| ch.is_ascii_hexdigit()));
    }

    #[test]
    fn quota_scope_differs_across_config_dirs() {
        let work = quota_scope_id(Some(OsStr::new("/tmp/.claude-work")), None, None).unwrap();
        let personal =
            quota_scope_id(Some(OsStr::new("/tmp/.claude-personal")), None, None).unwrap();
        assert_ne!(work, personal);
    }

    #[test]
    fn quota_scope_normalizes_tilde_and_relative_paths() {
        let home = Path::new("/Users/me");
        let cwd = Path::new("/Users/me/proj");
        let tilde = quota_scope_id(
            Some(OsStr::new("~/.claude-work")),
            Some(home.as_os_str()),
            Some(cwd),
        )
        .unwrap();
        let absolute = quota_scope_id(
            Some(OsStr::new("/Users/me/.claude-work")),
            Some(home.as_os_str()),
            Some(cwd),
        )
        .unwrap();
        let relative = quota_scope_id(
            Some(OsStr::new("../.claude-work")),
            Some(home.as_os_str()),
            Some(cwd),
        )
        .unwrap();
        assert_eq!(tilde, absolute);
        assert_eq!(relative, absolute);
    }

    #[test]
    fn quota_scope_never_embeds_the_config_path() {
        let path = "/Users/secret/.claude-work";
        let scope = quota_scope_id(Some(OsStr::new(path)), None, None).unwrap();
        assert!(!scope.contains("secret"));
        assert!(!scope.contains("claude"));
        assert!(!scope.contains('/'));
    }
}
