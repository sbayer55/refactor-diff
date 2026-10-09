//! User settings that outlive the server: which AI provider to use and how to reach it.
//!
//! Stored as `settings.json` in the same directory as the review marks, written with mode
//! 0600 because it holds API keys. The web API never returns a key: [`public_view`] masks
//! them, and a masked value sent back on save means "keep the stored key".

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Map, Value, json};

use crate::paths::{atomic_write, canonical_json, config_dir};

pub const PROVIDERS: &[&str] = &["claude", "openai", "ollama"];
pub const MASK: &str = "••••";
const SECRET_KEYS: &[&str] = &["api_key"];

pub fn defaults() -> Value {
    json!({
        "ai": {
            "provider": "claude",
            "providers": {
                "claude": {"api_key": "", "model": "claude-opus-5-5", "base_url": ""},
                "openai": {"base_url": "", "api_key": "", "model": ""},
                "ollama": {"host": "http://127.0.0.1:11434", "model": "", "num_ctx": 32768},
            },
            "context": {"function": true, "pr": true, "references": false},
        }
    })
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("{0}")]
pub struct SettingsError(pub String);

/// The settings document: the defaults with the user's values merged over them.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings(pub Value);

impl Settings {
    pub fn provider(&self) -> &str {
        self.0["ai"]["provider"].as_str().unwrap_or("claude")
    }

    /// One provider's configuration, as stored (an empty object for unknown providers).
    pub fn provider_cfg(&self, name: &str) -> Map<String, Value> {
        self.0["ai"]["providers"][name]
            .as_object()
            .cloned()
            .unwrap_or_default()
    }

    /// A string setting of a provider, `""` when absent or not a string.
    pub fn provider_str(&self, name: &str, key: &str) -> String {
        self.0["ai"]["providers"][name][key]
            .as_str()
            .unwrap_or("")
            .to_string()
    }

    /// Whether a context piece (`function`, `pr`, `references`) is on.
    pub fn context(&self, piece: &str) -> bool {
        truthy(&self.0["ai"]["context"][piece])
    }

    pub fn as_value(&self) -> &Value {
        &self.0
    }
}

/// Python truthiness of a JSON value.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn merge(base: &Value, over: &Value) -> Value {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            let mut out = b.clone();
            for (k, v) in o {
                let merged = match out.get(k) {
                    Some(existing) if existing.is_object() && v.is_object() => merge(existing, v),
                    _ => v.clone(),
                };
                out.insert(k.clone(), merged);
            }
            Value::Object(out)
        }
        _ => over.clone(),
    }
}

/// Reads and writes `settings.json` under one root directory.
pub struct SettingsStore {
    root: PathBuf,
    lock: Mutex<()>,
}

impl SettingsStore {
    /// `root` defaults to the XDG config dir.
    pub fn new(root: Option<&Path>) -> Self {
        Self {
            root: root.map(Path::to_path_buf).unwrap_or_else(config_dir),
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> PathBuf {
        self.root.join("settings.json")
    }

    /// The settings with every default filled in (never fails on a missing or broken file).
    pub fn load(&self) -> Settings {
        let data = std::fs::read_to_string(self.path())
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        let mut merged = merge(&defaults(), &data);
        let valid = merged["ai"]["provider"]
            .as_str()
            .is_some_and(|p| PROVIDERS.contains(&p));
        if !valid {
            merged["ai"]["provider"] = defaults()["ai"]["provider"].clone();
        }
        Settings(merged)
    }

    /// Write `data` (merged over the defaults) atomically with mode 0600; returns it.
    pub fn save(&self, data: &Value) -> std::io::Result<Settings> {
        let merged = merge(&defaults(), data);
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        atomic_write(&self.path(), &canonical_json(&merged), Some(0o600))?;
        Ok(Settings(merged))
    }
}

pub fn mask(secret: &str) -> String {
    if secret.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = secret.chars().collect();
    let tail: String = chars[chars.len().saturating_sub(4)..].iter().collect();
    format!("{MASK}{tail}")
}

pub fn is_masked(value: &Value) -> bool {
    value.as_str().is_some_and(|s| s.starts_with(MASK))
}

/// The settings with secrets masked and a `configured` flag per provider.
pub fn public_view(settings: &Settings) -> Value {
    let mut out = settings.0.clone();
    if let Some(providers) = out["ai"]["providers"].as_object_mut() {
        let names: Vec<String> = providers.keys().cloned().collect();
        for name in names {
            if let Some(cfg) = providers[&name].as_object_mut() {
                for key in SECRET_KEYS {
                    if let Some(v) = cfg.get_mut(*key) {
                        let masked = mask(v.as_str().unwrap_or(""));
                        *v = Value::String(masked);
                    }
                }
                cfg.insert(
                    "configured".into(),
                    Value::Bool(configured(settings, &name)),
                );
            }
        }
    }
    let active = settings.provider().to_string();
    out["ai"]["active"] = json!({
        "provider": active,
        "model": settings.0["ai"]["providers"][&active].get("model").cloned().unwrap_or_else(|| Value::String(String::new())),
        "configured": configured(settings, &active),
    });
    out
}

pub fn configured(settings: &Settings, provider: &str) -> bool {
    let cfg = &settings.0["ai"]["providers"][provider];
    match provider {
        "claude" => truthy(&cfg["api_key"]),
        "openai" => truthy(&cfg["base_url"]) && truthy(&cfg["model"]),
        "ollama" => truthy(&cfg["host"]) && truthy(&cfg["model"]),
        _ => false,
    }
}

/// Python's `repr()` of a JSON value, for error messages.
fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => refactor_diff_core::lang::pyrepr::py_repr_str(s),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        other => other.to_string(),
    }
}

/// `current` with the `ai` section of `update` applied; masked secrets keep the stored value,
/// so the UI can round-trip the public view.
pub fn apply_update(current: &Settings, update: &Value) -> Result<Settings, SettingsError> {
    let Some(ai) = update.get("ai").filter(|a| a.is_object()) else {
        return Err(SettingsError(
            "Expected an object with an 'ai' section.".into(),
        ));
    };
    let mut merged = current.0.clone();
    if let Some(provider) = ai.get("provider") {
        if !provider.as_str().is_some_and(|p| PROVIDERS.contains(&p)) {
            return Err(SettingsError(format!(
                "Unknown provider {}.",
                py_repr(provider)
            )));
        }
        merged["ai"]["provider"] = provider.clone();
    }
    if let Some(providers) = ai.get("providers").and_then(Value::as_object) {
        for (name, cfg) in providers {
            let Some(cfg) = cfg.as_object() else { continue };
            if !PROVIDERS.contains(&name.as_str()) {
                continue;
            }
            let target = merged["ai"]["providers"][name]
                .as_object_mut()
                .expect("defaults have every provider");
            for (key, value) in cfg {
                if key == "configured" {
                    continue;
                }
                if SECRET_KEYS.contains(&key.as_str()) && is_masked(value) {
                    continue;
                }
                let value = if key == "num_ctx" {
                    let n = py_int(value)
                        .ok_or_else(|| SettingsError("Context window must be a number.".into()))?;
                    Value::from(n.max(1024))
                } else if let Some(s) = value.as_str() {
                    Value::String(s.trim().to_string())
                } else {
                    return Err(SettingsError(format!("{name}.{key} must be text.")));
                };
                target.insert(key.clone(), value);
            }
        }
    }
    if let Some(ctx) = ai.get("context").and_then(Value::as_object) {
        let keys: Vec<String> = merged["ai"]["context"]
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        for key in keys {
            if let Some(v) = ctx.get(&key) {
                merged["ai"]["context"][&key] = Value::Bool(truthy(v));
            }
        }
    }
    Ok(Settings(merged))
}

/// Python `int(value)` for the values JSON can carry.
fn py_int(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64)),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        Value::Bool(b) => Some(*b as i64),
        _ => None,
    }
}

/// Provider config for a connection test: unsaved values from the dialog, with masked secrets
/// replaced by the stored ones.
pub fn resolve_test_config(
    current: &Settings,
    provider: &str,
    cfg: Option<&Value>,
) -> Result<Settings, SettingsError> {
    if !PROVIDERS.contains(&provider) {
        return Err(SettingsError(format!(
            "Unknown provider {}.",
            py_repr(&Value::String(provider.into()))
        )));
    }
    let probe = json!({"ai": {"provider": provider, "providers": {provider: cfg.cloned().unwrap_or_else(|| json!({}))}}});
    apply_update(current, &probe)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDENS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/goldens");

    #[test]
    fn defaults_when_missing_or_broken() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(Some(dir.path()));
        assert_eq!(store.load().0, defaults());
        std::fs::write(store.path(), "{not json").unwrap();
        assert_eq!(store.load().0, defaults());
        std::fs::write(store.path(), "[1]").unwrap();
        assert_eq!(store.load().0, defaults());
        std::fs::write(store.path(), r#"{"ai": {"provider": "bogus"}}"#).unwrap();
        assert_eq!(store.load().provider(), "claude");
    }

    #[test]
    fn save_matches_python_bytes_and_mode() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(Some(dir.path()));
        let data = json!({"ai": {"context": {"references": true}, "provider": "ollama", "providers": {"claude": {"api_key": "sk-ant-abcdef1234"}, "ollama": {"model": "llama3", "num_ctx": 65536}, "openai": {"base_url": "http://localhost:1/v1", "model": "m"}}}});
        store.save(&data).unwrap();
        let written = std::fs::read(store.path()).unwrap();
        let golden = std::fs::read(format!("{GOLDENS}/settings.sample.json")).unwrap();
        assert_eq!(
            String::from_utf8(written).unwrap(),
            String::from_utf8(golden).unwrap()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(store.path())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let public = public_view(&store.load());
        let golden: Value = serde_json::from_str(
            &std::fs::read_to_string(format!("{GOLDENS}/settings.public.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(public, golden);
    }

    #[test]
    fn apply_update_rules() {
        let current = Settings(defaults());
        let updated = apply_update(&current, &json!({"ai": {"provider": "ollama", "providers": {"ollama": {"model": "  m ", "num_ctx": "65536", "configured": true}, "claude": {"api_key": "••••1234"}, "bogus": {"x": 1}}, "context": {"pr": 0, "nope": true}}})).unwrap();
        assert_eq!(updated.provider(), "ollama");
        assert_eq!(updated.provider_str("ollama", "model"), "m");
        assert_eq!(updated.0["ai"]["providers"]["ollama"]["num_ctx"], 65536);
        assert_eq!(updated.provider_str("claude", "api_key"), "");
        assert!(!updated.context("pr"));
        assert!(updated.0["ai"]["context"].get("nope").is_none());
        assert_eq!(
            apply_update(
                &current,
                &json!({"ai": {"providers": {"ollama": {"num_ctx": "lots"}}}})
            )
            .unwrap_err()
            .0,
            "Context window must be a number."
        );
        assert_eq!(
            apply_update(
                &current,
                &json!({"ai": {"providers": {"ollama": {"host": 5}}}})
            )
            .unwrap_err()
            .0,
            "ollama.host must be text."
        );
        assert_eq!(
            apply_update(&current, &json!({"ai": {"provider": "cursor"}}))
                .unwrap_err()
                .0,
            "Unknown provider 'cursor'."
        );
        assert_eq!(
            apply_update(&current, &json!({"nope": 1})).unwrap_err().0,
            "Expected an object with an 'ai' section."
        );
        let stored = Settings(merge(
            &defaults(),
            &json!({"ai": {"providers": {"claude": {"api_key": "sk-real"}}}}),
        ));
        let kept = apply_update(
            &stored,
            &json!({"ai": {"providers": {"claude": {"api_key": "••••real", "model": "x"}}}}),
        )
        .unwrap();
        assert_eq!(kept.provider_str("claude", "api_key"), "sk-real");
        assert_eq!(kept.provider_str("claude", "model"), "x");
        let cleared = apply_update(
            &stored,
            &json!({"ai": {"providers": {"claude": {"api_key": ""}}}}),
        )
        .unwrap();
        assert_eq!(cleared.provider_str("claude", "api_key"), "");
        let probe = resolve_test_config(
            &stored,
            "claude",
            Some(&json!({"api_key": "••••real", "model": "m2"})),
        )
        .unwrap();
        assert_eq!(probe.provider(), "claude");
        assert_eq!(probe.provider_str("claude", "api_key"), "sk-real");
        assert_eq!(
            resolve_test_config(&stored, "nope", None).unwrap_err().0,
            "Unknown provider 'nope'."
        );
    }

    #[test]
    fn masking() {
        assert_eq!(mask(""), "");
        assert_eq!(mask("abc"), "••••abc");
        assert_eq!(mask("sk-ant-abcdef1234"), "••••1234");
        assert!(is_masked(&json!("••••x")));
        assert!(!is_masked(&json!("sk")));
    }
}
