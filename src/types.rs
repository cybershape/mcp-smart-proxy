use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;

use rmcp::model::Tool;
use serde::Serialize;
use serde_json::Value as JsonValue;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConfiguredServer {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub env_vars: Vec<String>,
}

impl ConfiguredServer {
    pub fn resolved_env(&self) -> Vec<(String, OsString)> {
        let mut resolved = BTreeMap::new();

        for name in &self.env_vars {
            if let Some(value) = env::var_os(name) {
                resolved.insert(name.clone(), value);
            }
        }

        for (name, value) in &self.env {
            resolved.insert(name.clone(), OsString::from(value));
        }

        resolved.into_iter().collect()
    }
}

#[derive(Debug, Clone)]
pub struct CodexRuntimeConfig {
    pub model: String,
}

#[derive(Debug, Clone)]
pub struct OpencodeRuntimeConfig {
    pub model: String,
}

#[derive(Debug, Clone)]
pub enum ModelProviderConfig {
    Codex(CodexRuntimeConfig),
    Opencode(OpencodeRuntimeConfig),
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct CachedTools {
    pub server: String,
    pub summary: String,
    pub fetched_at_epoch_ms: u128,
    pub tools: Vec<ToolSnapshot>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ToolSnapshot {
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub input_schema: JsonValue,
    pub output_schema: Option<JsonValue>,
    pub annotations: Option<JsonValue>,
    pub execution: Option<JsonValue>,
    pub icons: Option<JsonValue>,
    pub meta: Option<JsonValue>,
}

pub fn tool_snapshot(tool: &Tool) -> ToolSnapshot {
    ToolSnapshot {
        name: tool.name.to_string(),
        title: tool.title.clone(),
        description: tool.description.as_ref().map(ToString::to_string),
        input_schema: JsonValue::Object((*(tool.input_schema.clone())).clone()),
        output_schema: tool
            .output_schema
            .as_ref()
            .map(|schema| JsonValue::Object((**schema).clone())),
        annotations: tool.annotations.as_ref().map(json_value_or_null),
        execution: tool.execution.as_ref().map(json_value_or_null),
        icons: tool.icons.as_ref().map(json_value_or_null),
        meta: tool.meta.as_ref().map(json_value_or_null),
    }
}

fn json_value_or_null<T: Serialize>(value: &T) -> JsonValue {
    serde_json::to_value(value).unwrap_or(JsonValue::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn resolves_forwarded_and_static_env_vars() {
        let _guard = env_lock().lock().unwrap();
        let previous_forwarded = env::var("MSP_TEST_FORWARDED").ok();
        let previous_overridden = env::var("MSP_TEST_OVERRIDDEN").ok();

        unsafe {
            env::set_var("MSP_TEST_FORWARDED", "forwarded");
            env::set_var("MSP_TEST_OVERRIDDEN", "from-process");
        }

        let server = ConfiguredServer {
            command: "demo".to_string(),
            args: Vec::new(),
            env: BTreeMap::from([("MSP_TEST_OVERRIDDEN".to_string(), "from-config".to_string())]),
            env_vars: vec![
                "MSP_TEST_FORWARDED".to_string(),
                "MSP_TEST_OVERRIDDEN".to_string(),
                "MSP_TEST_MISSING".to_string(),
            ],
        };

        let resolved = server.resolved_env();

        assert_eq!(
            resolved,
            vec![
                ("MSP_TEST_FORWARDED".to_string(), "forwarded".into()),
                ("MSP_TEST_OVERRIDDEN".to_string(), "from-config".into()),
            ]
        );

        match previous_forwarded {
            Some(value) => unsafe { env::set_var("MSP_TEST_FORWARDED", value) },
            None => unsafe { env::remove_var("MSP_TEST_FORWARDED") },
        }
        match previous_overridden {
            Some(value) => unsafe { env::set_var("MSP_TEST_OVERRIDDEN", value) },
            None => unsafe { env::remove_var("MSP_TEST_OVERRIDDEN") },
        }
    }

    #[test]
    fn default_configured_server_has_no_env() {
        let server = ConfiguredServer::default();

        assert!(server.command.is_empty());
        assert!(server.args.is_empty());
        assert!(server.env.is_empty());
        assert!(server.env_vars.is_empty());
    }
}
