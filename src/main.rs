use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

use aisdk::{
    core::{DynamicModel, LanguageModelRequest},
    providers::OpenAI,
};
use clap::{Parser, Subcommand};
use rmcp::{
    ServiceExt,
    model::Tool,
    transport::{ConfigureCommandExt, TokioChildProcess},
};
use serde::Serialize;
use serde_json::Value as JsonValue;
use toml::{Table, Value};

mod mcp_server;

const DEFAULT_CONFIG_PATH: &str = "~/.config/mcp-smart-proxy/config.toml";
const DEFAULT_OPENAI_MODEL: &str = "gpt-5.2";

#[derive(Debug, Parser)]
#[command(version, about = "A smart MCP proxy")]
struct Cli {
    /// Override the config file path.
    #[arg(long, value_name = "PATH", default_value = DEFAULT_CONFIG_PATH)]
    config: PathBuf,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Add a stdio MCP server to the config file.
    Add {
        name: String,
        #[arg(required = true, num_args = 1.., trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Refresh cached tool metadata for a configured MCP server.
    Reload { name: String },
    /// Start a stdio MCP server that exposes cached toolset activation.
    Mcp,
    /// Update application configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Update OpenAI settings.
    Openai {
        #[arg(long)]
        baseurl: Option<String>,
        #[arg(long)]
        key: Option<String>,
        #[arg(long)]
        model: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConfiguredServer {
    command: String,
    args: Vec<String>,
}

#[derive(Debug, Clone)]
struct OpenAiRuntimeConfig {
    baseurl: Option<String>,
    key: String,
    model: String,
}

const OPENAI_API_BASE_ENV: &str = "OPENAI_API_BASE";
const OPENAI_API_KEY_ENV: &str = "OPENAI_API_KEY";

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
struct CachedTools {
    server: String,
    summary: String,
    fetched_at_epoch_ms: u128,
    tools: Vec<ToolSnapshot>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
struct ToolSnapshot {
    name: String,
    title: Option<String>,
    description: Option<String>,
    input_schema: JsonValue,
    output_schema: Option<JsonValue>,
    annotations: Option<JsonValue>,
    execution: Option<JsonValue>,
    icons: Option<JsonValue>,
    meta: Option<JsonValue>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    let config_path = expand_tilde(&cli.config)?;

    match cli.command {
        Some(Command::Add { name, command }) => {
            let server_name = add_server(&config_path, &name, command)?;
            println!(
                "Added stdio MCP server `{server_name}` to {}",
                config_path.display()
            );
        }
        Some(Command::Reload { name }) => {
            let cache_path = reload_server(&config_path, &name).await?;
            println!("Reloaded MCP server `{name}` into {}", cache_path.display());
        }
        Some(Command::Mcp) => {
            mcp_server::serve_cached_toolsets(&config_path).await?;
        }
        Some(Command::Config {
            command:
                ConfigCommand::Openai {
                    baseurl,
                    key,
                    model,
                },
        }) => {
            update_openai_config(
                &config_path,
                OpenAiConfigUpdate {
                    baseurl,
                    key,
                    model,
                },
            )?;
            println!("Updated OpenAI config in {}", config_path.display());
        }
        None => {
            let _config = if config_path.exists() {
                Some(load_config_table(&config_path)?)
            } else {
                None
            };
        }
    }

    Ok(())
}

fn add_server(
    config_path: &Path,
    name: &str,
    raw_command: Vec<String>,
) -> Result<String, Box<dyn Error>> {
    let normalized = normalize_add_command(raw_command);
    let server = StdioServer::from_command(normalized)?;

    let mut config = load_config_table(config_path)?;
    let name = sanitize_name(name);
    if name.is_empty() {
        return Err("server name must contain at least one ASCII letter or digit".into());
    }
    if has_server_name(&config, &name) {
        return Err(format!("server `{name}` already exists").into());
    }

    insert_server(&mut config, &name, &server)?;
    save_config_table(config_path, &config)?;

    Ok(name)
}

fn normalize_add_command(raw_command: Vec<String>) -> Vec<String> {
    if raw_command.len() == 1 && looks_like_url(&raw_command[0]) {
        return vec![
            "npx".to_string(),
            "-y".to_string(),
            "mcp-remote".to_string(),
            raw_command[0].clone(),
        ];
    }

    raw_command
}

async fn reload_server(config_path: &Path, name: &str) -> Result<PathBuf, Box<dyn Error>> {
    let config = load_config_table(config_path)?;
    let (resolved_name, server) = configured_server(&config, name)?;
    let tools = fetch_tools(&server).await?;
    let openai = load_openai_runtime_config(&config)?;
    let summary = summarize_tools(&openai, &resolved_name, &tools).await?;
    let cache_path = cache_file_path(&resolved_name)?;
    let payload = CachedTools {
        server: resolved_name,
        summary,
        fetched_at_epoch_ms: unix_epoch_ms()?,
        tools: tools.iter().map(tool_snapshot).collect(),
    };

    write_cache(&cache_path, &payload)?;
    Ok(cache_path)
}

struct OpenAiConfigUpdate {
    baseurl: Option<String>,
    key: Option<String>,
    model: Option<String>,
}

fn update_openai_config(
    config_path: &Path,
    update: OpenAiConfigUpdate,
) -> Result<(), Box<dyn Error>> {
    let mut config = load_config_table(config_path)?;
    let openai_value = config
        .entry("openai")
        .or_insert_with(|| Value::Table(Table::new()));
    let openai = openai_value
        .as_table_mut()
        .ok_or_else(|| "`openai` in config must be a table".to_string())?;

    set_optional_string(openai, "baseurl", update.baseurl);
    set_optional_string(openai, "key", update.key);
    set_optional_string(openai, "model", update.model);

    save_config_table(config_path, &config)?;
    Ok(())
}

fn set_optional_string(table: &mut Table, key: &str, value: Option<String>) {
    if let Some(value) = value {
        table.insert(key.to_string(), Value::String(value));
    }
}

fn looks_like_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

fn configured_server(
    config: &Table,
    requested_name: &str,
) -> Result<(String, ConfiguredServer), Box<dyn Error>> {
    let servers = config
        .get("servers")
        .and_then(Value::as_table)
        .ok_or_else(|| "no `servers` table found in config".to_string())?;

    let resolved_name = if servers.contains_key(requested_name) {
        requested_name.to_string()
    } else {
        let normalized = sanitize_name(requested_name);
        if servers.contains_key(&normalized) {
            normalized
        } else {
            return Err(format!("server `{requested_name}` not found").into());
        }
    };

    let server = servers[&resolved_name]
        .as_table()
        .ok_or_else(|| format!("server `{resolved_name}` must be a table"))?;

    let transport = server
        .get("transport")
        .and_then(Value::as_str)
        .unwrap_or("stdio");
    if transport != "stdio" {
        return Err(format!(
            "server `{resolved_name}` uses unsupported transport `{transport}`, only `stdio` is supported"
        )
        .into());
    }

    let command = server
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("server `{resolved_name}` is missing `command`"))?
        .to_string();

    let args = server
        .get("args")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|value| {
                    value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                        format!("server `{resolved_name}` contains a non-string arg")
                    })
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();

    Ok((resolved_name, ConfiguredServer { command, args }))
}

async fn fetch_tools(server: &ConfiguredServer) -> Result<Vec<Tool>, Box<dyn Error>> {
    let transport = TokioChildProcess::builder(
        tokio::process::Command::new(&server.command).configure(|cmd| {
            cmd.args(&server.args);
        }),
    )
    .stderr(Stdio::inherit())
    .spawn()?
    .0;

    let client = ().serve(transport).await?;
    let tools = client.list_all_tools().await?;
    client.cancel().await?;
    Ok(tools)
}

fn load_openai_runtime_config(config: &Table) -> Result<OpenAiRuntimeConfig, Box<dyn Error>> {
    let table = config.get("openai").and_then(Value::as_table);

    let baseurl = openai_optional_string(table, "baseurl", Some(OPENAI_API_BASE_ENV));
    let key = openai_string(table, "key", Some(OPENAI_API_KEY_ENV))?;
    let model = openai_optional_string(table, "model", None)
        .unwrap_or_else(|| DEFAULT_OPENAI_MODEL.to_string());

    Ok(OpenAiRuntimeConfig {
        baseurl,
        key,
        model,
    })
}

fn openai_string(
    table: Option<&Table>,
    key: &str,
    env_key: Option<&str>,
) -> Result<String, Box<dyn Error>> {
    openai_optional_string(table, key, env_key).ok_or_else(|| {
        let message = match env_key {
            Some(env_key) => {
                format!("missing `openai.{key}` in config and `{env_key}` in environment")
            }
            None => format!("missing `openai.{key}` in config"),
        };

        message.into()
    })
}

fn openai_optional_string(
    table: Option<&Table>,
    key: &str,
    env_key: Option<&str>,
) -> Option<String> {
    if let Some(value) = table
        .and_then(|table| table.get(key))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        return Some(value.to_string());
    }

    env_key.and_then(|env_key| match env::var(env_key) {
        Ok(value) if !value.is_empty() => Some(value),
        _ => None,
    })
}

async fn summarize_tools(
    openai: &OpenAiRuntimeConfig,
    server_name: &str,
    tools: &[Tool],
) -> Result<String, Box<dyn Error>> {
    let tools_json = serde_json::to_string_pretty(&tools)?;
    let mut model_builder = OpenAI::<DynamicModel>::builder()
        .model_name(openai.model.clone())
        .api_key(openai.key.clone());
    if let Some(baseurl) = &openai.baseurl {
        model_builder = model_builder.base_url(baseurl.clone());
    }
    let model = model_builder.build()?;

    let prompt = format!(
        "You are summarizing an MCP toolset for another AI.\n\
Server name: {server_name}\n\
Return exactly one concise English sentence.\n\
The sentence must explain when this toolset should be activated, based on the available tools.\n\
Do not mention implementation details like MCP, JSON, schema, or caching unless essential.\n\
If the tools cover multiple related workflows, summarize the common decision boundary.\n\n\
Tools:\n{tools_json}"
    );

    let mut request = LanguageModelRequest::builder()
        .model(model)
        .prompt(prompt)
        .build();
    let response = request.generate_text().await?;
    let summary_text = response.text();
    let summary = summary_text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| "OpenAI returned an empty summary".to_string())?;

    Ok(summary.to_string())
}

fn cache_file_path(server_name: &str) -> Result<PathBuf, Box<dyn Error>> {
    cache_file_path_from_home(&home_dir()?, server_name)
}

fn cache_file_path_from_home(home: &Path, server_name: &str) -> Result<PathBuf, Box<dyn Error>> {
    Ok(home
        .join(".cache/mcp-smart-proxy")
        .join(format!("{server_name}.json")))
}

fn write_cache(path: &Path, payload: &CachedTools) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let contents = serde_json::to_string_pretty(payload)?;
    fs::write(path, contents)?;
    Ok(())
}

fn tool_snapshot(tool: &Tool) -> ToolSnapshot {
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

fn unix_epoch_ms() -> Result<u128, Box<dyn Error>> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StdioServer {
    command: String,
    args: Vec<String>,
}

impl StdioServer {
    fn from_command(command: Vec<String>) -> Result<Self, Box<dyn Error>> {
        let mut parts = command.into_iter();
        let executable = parts
            .next()
            .ok_or_else(|| "missing stdio server command".to_string())?;

        Ok(Self {
            command: executable,
            args: parts.collect(),
        })
    }
}

fn sanitize_name(value: &str) -> String {
    let mut result = String::new();
    let mut previous_dash = false;

    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            result.push(ch.to_ascii_lowercase());
            previous_dash = false;
        } else if !previous_dash {
            result.push('-');
            previous_dash = true;
        }
    }

    result.trim_matches('-').to_string()
}

fn load_config_table(path: &Path) -> Result<Table, Box<dyn Error>> {
    if !path.exists() {
        return Ok(Table::new());
    }

    let contents = fs::read_to_string(path)?;
    let table = toml::from_str(&contents)?;
    Ok(table)
}

fn save_config_table(path: &Path, config: &Table) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let contents = toml::to_string_pretty(config)?;
    fs::write(path, contents)?;
    Ok(())
}

fn has_server_name(config: &Table, name: &str) -> bool {
    config
        .get("servers")
        .and_then(Value::as_table)
        .map(|servers| servers.contains_key(name))
        .unwrap_or(false)
}

fn insert_server(
    config: &mut Table,
    name: &str,
    server: &StdioServer,
) -> Result<(), Box<dyn Error>> {
    let servers_value = config
        .entry("servers")
        .or_insert_with(|| Value::Table(Table::new()));
    let servers = servers_value
        .as_table_mut()
        .ok_or_else(|| "`servers` in config must be a table".to_string())?;

    let mut server_table = Table::new();
    server_table.insert("transport".to_string(), Value::String("stdio".to_string()));
    server_table.insert("command".to_string(), Value::String(server.command.clone()));
    server_table.insert(
        "args".to_string(),
        Value::Array(server.args.iter().cloned().map(Value::String).collect()),
    );

    servers.insert(name.to_string(), Value::Table(server_table));
    Ok(())
}

fn expand_tilde(path: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let path_str = path.to_string_lossy();

    if path_str == "~" {
        return Ok(home_dir()?);
    }

    if let Some(stripped) = path_str.strip_prefix("~/") {
        return Ok(home_dir()?.join(stripped));
    }

    Ok(path.to_path_buf())
}

fn home_dir() -> Result<PathBuf, Box<dyn Error>> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn with_openai_env<T>(base: Option<&str>, key: Option<&str>, test: impl FnOnce() -> T) -> T {
        let _guard = env_lock().lock().unwrap();
        let previous_base = env::var(OPENAI_API_BASE_ENV).ok();
        let previous_key = env::var(OPENAI_API_KEY_ENV).ok();

        match base {
            Some(value) => unsafe { env::set_var(OPENAI_API_BASE_ENV, value) },
            None => unsafe { env::remove_var(OPENAI_API_BASE_ENV) },
        }
        match key {
            Some(value) => unsafe { env::set_var(OPENAI_API_KEY_ENV, value) },
            None => unsafe { env::remove_var(OPENAI_API_KEY_ENV) },
        }

        let result = test();

        match previous_base {
            Some(value) => unsafe { env::set_var(OPENAI_API_BASE_ENV, value) },
            None => unsafe { env::remove_var(OPENAI_API_BASE_ENV) },
        }
        match previous_key {
            Some(value) => unsafe { env::set_var(OPENAI_API_KEY_ENV, value) },
            None => unsafe { env::remove_var(OPENAI_API_KEY_ENV) },
        }

        result
    }

    #[test]
    fn expands_default_config_path() {
        let home = PathBuf::from("/tmp/mcp-smart-proxy-home");
        unsafe {
            env::set_var("HOME", &home);
        }

        let expanded = expand_tilde(Path::new(DEFAULT_CONFIG_PATH)).unwrap();

        assert_eq!(expanded, home.join(".config/mcp-smart-proxy/config.toml"));
    }

    #[test]
    fn keeps_non_tilde_paths() {
        let path = PathBuf::from("/tmp/config.toml");

        let expanded = expand_tilde(&path).unwrap();

        assert_eq!(expanded, path);
    }

    #[test]
    fn parses_arbitrary_toml_content() {
        let config: Table = toml::from_str(
            r#"
                listen_addr = "127.0.0.1:8080"

                [upstream]
                url = "https://example.com/mcp"
            "#,
        )
        .unwrap();

        assert_eq!(config["listen_addr"].as_str(), Some("127.0.0.1:8080"));
        assert_eq!(
            config["upstream"]
                .as_table()
                .and_then(|table| table["url"].as_str()),
            Some("https://example.com/mcp")
        );
    }

    #[test]
    fn normalizes_bare_url_add_command() {
        assert_eq!(
            normalize_add_command(vec!["https://ones.com/mcp".to_string()]),
            vec![
                "npx".to_string(),
                "-y".to_string(),
                "mcp-remote".to_string(),
                "https://ones.com/mcp".to_string()
            ]
        );
    }

    #[test]
    fn sanitizes_server_name() {
        assert_eq!(sanitize_name("Ones MCP"), "ones-mcp");
    }

    #[test]
    fn writes_stdio_server_to_config() {
        let config_path = unique_test_path("write-server-config.toml");
        let server_name = add_server(
            &config_path,
            "ones",
            vec!["https://ones.com/mcp".to_string()],
        )
        .unwrap();
        let config = load_config_table(&config_path).unwrap();

        let saved = config["servers"][&server_name].as_table().unwrap();
        assert_eq!(saved["transport"].as_str(), Some("stdio"));
        assert_eq!(saved["command"].as_str(), Some("npx"));
        assert_eq!(
            saved["args"].as_array().unwrap(),
            &vec![
                Value::String("-y".to_string()),
                Value::String("mcp-remote".to_string()),
                Value::String("https://ones.com/mcp".to_string()),
            ]
        );

        fs::remove_file(config_path).unwrap();
    }

    #[test]
    fn rejects_duplicate_server_name() {
        let config_path = unique_test_path("duplicate-server-config.toml");
        add_server(
            &config_path,
            "ones",
            vec!["https://ones.com/mcp".to_string()],
        )
        .unwrap();

        let error = add_server(
            &config_path,
            "ones",
            vec!["https://example.com/mcp".to_string()],
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "server `ones` already exists");
        fs::remove_file(config_path).unwrap();
    }

    #[test]
    fn updates_openai_config_with_partial_fields() {
        let config_path = unique_test_path("openai-config.toml");
        update_openai_config(
            &config_path,
            OpenAiConfigUpdate {
                baseurl: Some("https://api.example.com/v1".to_string()),
                key: None,
                model: Some("gpt-4.1-mini".to_string()),
            },
        )
        .unwrap();

        let config = load_config_table(&config_path).unwrap();
        let openai = config["openai"].as_table().unwrap();

        assert_eq!(
            openai["baseurl"].as_str(),
            Some("https://api.example.com/v1")
        );
        assert_eq!(openai["model"].as_str(), Some("gpt-4.1-mini"));
        assert!(openai.get("key").is_none());

        fs::remove_file(config_path).unwrap();
    }

    #[test]
    fn preserves_existing_openai_fields_when_updating_subset() {
        let config_path = unique_test_path("openai-config-preserve.toml");
        update_openai_config(
            &config_path,
            OpenAiConfigUpdate {
                baseurl: Some("https://api.example.com/v1".to_string()),
                key: Some("sk-old".to_string()),
                model: Some("gpt-4.1".to_string()),
            },
        )
        .unwrap();
        update_openai_config(
            &config_path,
            OpenAiConfigUpdate {
                baseurl: None,
                key: Some("sk-new".to_string()),
                model: None,
            },
        )
        .unwrap();

        let config = load_config_table(&config_path).unwrap();
        let openai = config["openai"].as_table().unwrap();

        assert_eq!(
            openai["baseurl"].as_str(),
            Some("https://api.example.com/v1")
        );
        assert_eq!(openai["key"].as_str(), Some("sk-new"));
        assert_eq!(openai["model"].as_str(), Some("gpt-4.1"));

        fs::remove_file(config_path).unwrap();
    }

    #[test]
    fn loads_openai_base_and_key_from_environment_when_config_is_missing_them() {
        with_openai_env(Some("https://env.example.com/v1"), Some("sk-env"), || {
            let config: Table = toml::from_str(
                r#"
                        [openai]
                        model = "gpt-4.1-mini"
                    "#,
            )
            .unwrap();

            let runtime = load_openai_runtime_config(&config).unwrap();

            assert_eq!(
                runtime.baseurl.as_deref(),
                Some("https://env.example.com/v1")
            );
            assert_eq!(runtime.key, "sk-env");
            assert_eq!(runtime.model, "gpt-4.1-mini");
        });
    }

    #[test]
    fn prefers_openai_config_file_over_environment_variables() {
        with_openai_env(Some("https://env.example.com/v1"), Some("sk-env"), || {
            let config: Table = toml::from_str(
                r#"
                        [openai]
                        baseurl = "https://config.example.com/v1"
                        key = "sk-config"
                        model = "gpt-4.1"
                    "#,
            )
            .unwrap();

            let runtime = load_openai_runtime_config(&config).unwrap();

            assert_eq!(
                runtime.baseurl.as_deref(),
                Some("https://config.example.com/v1")
            );
            assert_eq!(runtime.key, "sk-config");
            assert_eq!(runtime.model, "gpt-4.1");
        });
    }

    #[test]
    fn allows_missing_openai_baseurl_when_no_config_or_env_value_exists() {
        with_openai_env(None, Some("sk-env"), || {
            let config: Table = toml::from_str(
                r#"
                    [openai]
                    model = "gpt-4.1-mini"
                "#,
            )
            .unwrap();

            let runtime = load_openai_runtime_config(&config).unwrap();

            assert_eq!(runtime.baseurl, None);
            assert_eq!(runtime.key, "sk-env");
            assert_eq!(runtime.model, "gpt-4.1-mini");
        });
    }

    #[test]
    fn uses_default_openai_model_when_config_is_missing_it() {
        with_openai_env(None, Some("sk-env"), || {
            let config: Table = toml::from_str(
                r#"
                    [openai]
                "#,
            )
            .unwrap();

            let runtime = load_openai_runtime_config(&config).unwrap();

            assert_eq!(runtime.baseurl, None);
            assert_eq!(runtime.key, "sk-env");
            assert_eq!(runtime.model, DEFAULT_OPENAI_MODEL);
        });
    }

    #[test]
    fn requires_openai_key_in_config_or_environment() {
        with_openai_env(None, None, || {
            let config: Table = toml::from_str(
                r#"
                    [openai]
                "#,
            )
            .unwrap();

            let error = load_openai_runtime_config(&config).unwrap_err();

            assert_eq!(
                error.to_string(),
                "missing `openai.key` in config and `OPENAI_API_KEY` in environment"
            );
        });
    }

    #[test]
    fn finds_server_by_exact_or_sanitized_name() {
        let config: Table = toml::from_str(
            r#"
                [servers.my-server]
                transport = "stdio"
                command = "uvx"
                args = ["mcp-server"]
            "#,
        )
        .unwrap();

        let (exact_name, exact_server) = configured_server(&config, "my-server").unwrap();
        assert_eq!(exact_name, "my-server");
        assert_eq!(
            exact_server,
            ConfiguredServer {
                command: "uvx".to_string(),
                args: vec!["mcp-server".to_string()],
            }
        );

        let (sanitized_name, _) = configured_server(&config, "My Server").unwrap();
        assert_eq!(sanitized_name, "my-server");
    }

    #[test]
    fn builds_cache_file_path_under_default_cache_dir() {
        let home = PathBuf::from("/tmp/mcp-smart-proxy-cache-home");
        let path = cache_file_path_from_home(&home, "demo-server").unwrap();

        assert_eq!(path, home.join(".cache/mcp-smart-proxy/demo-server.json"));
    }

    fn unique_test_path(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        env::temp_dir().join(format!("mcp-smart-proxy-{unique}-{name}"))
    }
}
