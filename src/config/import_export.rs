use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::Path;

use serde_json::{Map as JsonMap, Value as JsonValue};
use toml::{Table, Value};

use crate::env_template::collect_env_var_names;
use crate::fs_util::write_file_atomically;
use crate::paths::{format_path_for_display, sibling_backup_path};

use super::local::{
    load_config_table, merge_env_vars, parse_json_string_object, parse_toml_string_array,
    parse_toml_string_table, save_config_table,
};
use super::provider::{claude_config_path, codex_config_path, opencode_config_path};
use super::self_server::{
    claude_server_raw_command, codex_server_raw_command, inspect_claude_self_server,
    inspect_codex_self_server, inspect_opencode_self_server, next_available_server_name,
    opencode_server_raw_command, proxy_stdio_server,
};
use super::{
    CLAUDE_PROVIDER_NAME, CODEX_PROVIDER_NAME, ImportPlan, ImportableServer,
    ImportedServerDefinition, InstallMcpServerResult, InstallMcpServerStatus,
    ReplaceMcpServersResult, RestoreMcpServersResult, StdioServer, is_self_server_command,
};

pub fn load_codex_servers_for_import() -> Result<(std::path::PathBuf, ImportPlan), Box<dyn Error>> {
    let path = codex_config_path()?;
    let plan = load_codex_servers_for_import_from_path(&path)?;
    Ok((path, plan))
}

pub fn load_opencode_servers_for_import() -> Result<(std::path::PathBuf, ImportPlan), Box<dyn Error>>
{
    let path = opencode_config_path()?;
    let plan = load_opencode_servers_for_import_from_path(&path)?;
    Ok((path, plan))
}

pub fn load_claude_servers_for_import() -> Result<(std::path::PathBuf, ImportPlan), Box<dyn Error>>
{
    let path = claude_config_path()?;
    let plan = load_claude_servers_for_import_from_path(&path)?;
    Ok((path, plan))
}

pub fn install_codex_mcp_server() -> Result<InstallMcpServerResult, Box<dyn Error>> {
    let config_path = codex_config_path()?;
    let mut config = load_config_table(&config_path)?;
    let desired_server = proxy_stdio_server(CODEX_PROVIDER_NAME);

    let (name, status) = {
        let servers_value = config
            .entry("mcp_servers")
            .or_insert_with(|| Value::Table(Table::new()));
        let servers = servers_value
            .as_table_mut()
            .ok_or_else(|| "`mcp_servers` in Codex config must be a table".to_string())?;

        match inspect_codex_self_server(servers, CODEX_PROVIDER_NAME) {
            Some((name, true)) => {
                return Ok(InstallMcpServerResult {
                    name,
                    config_path,
                    status: InstallMcpServerStatus::AlreadyInstalled,
                });
            }
            Some((name, false)) => {
                servers.insert(name.clone(), codex_server_value(&desired_server));
                (name, InstallMcpServerStatus::Updated)
            }
            None => {
                let name = next_available_server_name(servers.keys().map(String::as_str));
                servers.insert(name.clone(), codex_server_value(&desired_server));
                (name, InstallMcpServerStatus::Installed)
            }
        }
    };

    save_config_table(&config_path, &config)?;

    Ok(InstallMcpServerResult {
        name,
        config_path,
        status,
    })
}

pub fn install_opencode_mcp_server() -> Result<InstallMcpServerResult, Box<dyn Error>> {
    let config_path = opencode_config_path()?;
    let mut config = load_opencode_config(&config_path)?;
    let desired_server = proxy_stdio_server(super::OPENCODE_PROVIDER_NAME);

    let (name, status) = {
        let root = config
            .as_object_mut()
            .ok_or_else(|| "OpenCode config root must be a JSON object".to_string())?;
        let servers_value = root
            .entry("mcp".to_string())
            .or_insert_with(|| JsonValue::Object(JsonMap::new()));
        let servers = servers_value
            .as_object_mut()
            .ok_or_else(|| "`mcp` in OpenCode config must be an object".to_string())?;

        match inspect_opencode_self_server(servers, super::OPENCODE_PROVIDER_NAME) {
            Some((name, true)) => {
                return Ok(InstallMcpServerResult {
                    name,
                    config_path,
                    status: InstallMcpServerStatus::AlreadyInstalled,
                });
            }
            Some((name, false)) => {
                servers.insert(name.clone(), opencode_server_value(&desired_server));
                (name, InstallMcpServerStatus::Updated)
            }
            None => {
                let name = next_available_server_name(servers.keys().map(String::as_str));
                servers.insert(name.clone(), opencode_server_value(&desired_server));
                (name, InstallMcpServerStatus::Installed)
            }
        }
    };

    save_opencode_config(&config_path, &config)?;

    Ok(InstallMcpServerResult {
        name,
        config_path,
        status,
    })
}

pub fn install_claude_mcp_server() -> Result<InstallMcpServerResult, Box<dyn Error>> {
    let config_path = claude_config_path()?;
    let mut config = load_claude_config(&config_path)?;
    let desired_server = proxy_stdio_server(CLAUDE_PROVIDER_NAME);

    let (name, status) = {
        let root = config
            .as_object_mut()
            .ok_or_else(|| "Claude Code config root must be a JSON object".to_string())?;
        let servers_value = root
            .entry("mcpServers".to_string())
            .or_insert_with(|| JsonValue::Object(JsonMap::new()));
        let servers = servers_value
            .as_object_mut()
            .ok_or_else(|| "`mcpServers` in Claude Code config must be an object".to_string())?;

        match inspect_claude_self_server(servers, CLAUDE_PROVIDER_NAME) {
            Some((name, true)) => {
                return Ok(InstallMcpServerResult {
                    name,
                    config_path,
                    status: InstallMcpServerStatus::AlreadyInstalled,
                });
            }
            Some((name, false)) => {
                servers.insert(name.clone(), claude_server_value(&desired_server));
                (name, InstallMcpServerStatus::Updated)
            }
            None => {
                let name = next_available_server_name(servers.keys().map(String::as_str));
                servers.insert(name.clone(), claude_server_value(&desired_server));
                (name, InstallMcpServerStatus::Installed)
            }
        }
    };

    save_claude_config(&config_path, &config)?;

    Ok(InstallMcpServerResult {
        name,
        config_path,
        status,
    })
}

pub fn replace_codex_mcp_servers() -> Result<ReplaceMcpServersResult, Box<dyn Error>> {
    let config_path = codex_config_path()?;
    replace_codex_mcp_servers_from_path(&config_path)
}

pub fn replace_opencode_mcp_servers() -> Result<ReplaceMcpServersResult, Box<dyn Error>> {
    let config_path = opencode_config_path()?;
    replace_opencode_mcp_servers_from_path(&config_path)
}

pub fn replace_claude_mcp_servers() -> Result<ReplaceMcpServersResult, Box<dyn Error>> {
    let config_path = claude_config_path()?;
    replace_claude_mcp_servers_from_path(&config_path)
}

pub fn restore_codex_mcp_servers() -> Result<RestoreMcpServersResult, Box<dyn Error>> {
    let config_path = codex_config_path()?;
    restore_codex_mcp_servers_from_path(&config_path)
}

pub fn restore_opencode_mcp_servers() -> Result<RestoreMcpServersResult, Box<dyn Error>> {
    let config_path = opencode_config_path()?;
    restore_opencode_mcp_servers_from_path(&config_path)
}

pub fn restore_claude_mcp_servers() -> Result<RestoreMcpServersResult, Box<dyn Error>> {
    let config_path = claude_config_path()?;
    restore_claude_mcp_servers_from_path(&config_path)
}

pub(crate) fn replace_codex_mcp_servers_from_path(
    config_path: &Path,
) -> Result<ReplaceMcpServersResult, Box<dyn Error>> {
    let mut config = load_config_table(config_path)?;
    let existing_servers = match config.get("mcp_servers") {
        None => Table::new(),
        Some(Value::Table(servers)) => servers.clone(),
        Some(_) => return Err("`mcp_servers` in Codex config must be a table".into()),
    };
    let backup_path = sibling_backup_path(config_path, "msp-backup");

    merge_codex_servers_into_backup(&backup_path, &existing_servers)?;

    if config.remove("mcp_servers").is_some() {
        save_config_table(config_path, &config)?;
    }

    Ok(ReplaceMcpServersResult {
        config_path: config_path.to_path_buf(),
        backup_path,
        backed_up_server_count: existing_servers.len(),
        removed_server_count: existing_servers.len(),
    })
}

pub(crate) fn replace_opencode_mcp_servers_from_path(
    config_path: &Path,
) -> Result<ReplaceMcpServersResult, Box<dyn Error>> {
    let mut config = load_opencode_config(config_path)?;
    let root = config
        .as_object_mut()
        .ok_or_else(|| "OpenCode config root must be a JSON object".to_string())?;
    let existing_servers = match root.get("mcp") {
        None => JsonMap::new(),
        Some(JsonValue::Object(servers)) => servers.clone(),
        Some(_) => return Err("`mcp` in OpenCode config must be an object".into()),
    };
    let backup_path = sibling_backup_path(config_path, "msp-backup");

    merge_opencode_servers_into_backup(&backup_path, &existing_servers)?;

    if root.remove("mcp").is_some() {
        save_opencode_config(config_path, &config)?;
    }

    Ok(ReplaceMcpServersResult {
        config_path: config_path.to_path_buf(),
        backup_path,
        backed_up_server_count: existing_servers.len(),
        removed_server_count: existing_servers.len(),
    })
}

pub(crate) fn replace_claude_mcp_servers_from_path(
    config_path: &Path,
) -> Result<ReplaceMcpServersResult, Box<dyn Error>> {
    let mut config = load_claude_config(config_path)?;
    let root = config
        .as_object_mut()
        .ok_or_else(|| "Claude Code config root must be a JSON object".to_string())?;
    let existing_servers = match root.get("mcpServers") {
        None => JsonMap::new(),
        Some(JsonValue::Object(servers)) => servers.clone(),
        Some(_) => return Err("`mcpServers` in Claude Code config must be an object".into()),
    };
    let backup_path = sibling_backup_path(config_path, "msp-backup");

    merge_claude_servers_into_backup(&backup_path, &existing_servers)?;

    if root.remove("mcpServers").is_some() {
        save_claude_config(config_path, &config)?;
    }

    Ok(ReplaceMcpServersResult {
        config_path: config_path.to_path_buf(),
        backup_path,
        backed_up_server_count: existing_servers.len(),
        removed_server_count: existing_servers.len(),
    })
}

pub(crate) fn restore_codex_mcp_servers_from_path(
    config_path: &Path,
) -> Result<RestoreMcpServersResult, Box<dyn Error>> {
    let backup_path = sibling_backup_path(config_path, "msp-backup");
    let backup = load_required_codex_backup(&backup_path)?;
    let restored_servers = backup
        .get("mcp_servers")
        .and_then(Value::as_table)
        .ok_or_else(|| {
            format!(
                "no `mcp_servers` table found in Codex backup {}",
                format_path_for_display(&backup_path)
            )
        })?
        .clone();

    let mut config = load_config_table(config_path)?;
    let removed_self_server_count = remove_codex_self_servers(&mut config)?;
    merge_codex_servers_into_target(&mut config, &restored_servers)?;
    save_config_table(config_path, &config)?;

    Ok(RestoreMcpServersResult {
        config_path: config_path.to_path_buf(),
        backup_path,
        removed_self_server_count,
        restored_server_count: restored_servers.len(),
    })
}

pub(crate) fn restore_opencode_mcp_servers_from_path(
    config_path: &Path,
) -> Result<RestoreMcpServersResult, Box<dyn Error>> {
    let backup_path = sibling_backup_path(config_path, "msp-backup");
    let backup = load_required_opencode_backup(&backup_path)?;
    let restored_servers = backup
        .get("mcp")
        .and_then(JsonValue::as_object)
        .ok_or_else(|| {
            format!(
                "no `mcp` object found in OpenCode backup {}",
                format_path_for_display(&backup_path)
            )
        })?
        .clone();

    let mut config = load_opencode_config(config_path)?;
    let removed_self_server_count = remove_opencode_self_servers(&mut config)?;
    merge_opencode_servers_into_target(&mut config, &restored_servers)?;
    save_opencode_config(config_path, &config)?;

    Ok(RestoreMcpServersResult {
        config_path: config_path.to_path_buf(),
        backup_path,
        removed_self_server_count,
        restored_server_count: restored_servers.len(),
    })
}

pub(crate) fn restore_claude_mcp_servers_from_path(
    config_path: &Path,
) -> Result<RestoreMcpServersResult, Box<dyn Error>> {
    let backup_path = sibling_backup_path(config_path, "msp-backup");
    let backup = load_required_claude_backup(&backup_path)?;
    let restored_servers = backup
        .get("mcpServers")
        .and_then(JsonValue::as_object)
        .ok_or_else(|| {
            format!(
                "no `mcpServers` object found in Claude Code backup {}",
                format_path_for_display(&backup_path)
            )
        })?
        .clone();

    let mut config = load_claude_config(config_path)?;
    let removed_self_server_count = remove_claude_self_servers(&mut config)?;
    merge_claude_servers_into_target(&mut config, &restored_servers)?;
    save_claude_config(config_path, &config)?;

    Ok(RestoreMcpServersResult {
        config_path: config_path.to_path_buf(),
        backup_path,
        removed_self_server_count,
        restored_server_count: restored_servers.len(),
    })
}

pub(crate) fn load_codex_servers_for_import_from_path(
    path: &Path,
) -> Result<ImportPlan, Box<dyn Error>> {
    if !path.exists() {
        return Err(format!(
            "Codex config not found at {}",
            format_path_for_display(path)
        )
        .into());
    }

    let config = load_config_table(path)?;
    let servers = config
        .get("mcp_servers")
        .and_then(Value::as_table)
        .ok_or_else(|| {
            format!(
                "no `mcp_servers` table found in Codex config {}",
                format_path_for_display(path)
            )
        })?;

    if servers.is_empty() {
        return Err(format!(
            "no MCP servers found in Codex config {}",
            format_path_for_display(path)
        )
        .into());
    }

    let mut names = servers.keys().cloned().collect::<Vec<_>>();
    names.sort();

    let mut importable_servers = Vec::new();
    let mut skipped_self_servers = Vec::new();

    for name in names {
        let server = servers[&name]
            .as_table()
            .ok_or_else(|| format!("Codex MCP server `{name}` must be a table"))?;
        validate_importable_codex_server(&name, server)?;
        let enabled = parse_codex_import_server_enabled(server, &name)?;
        let imported = codex_imported_server_command(server, &name)?;

        if imported.url.is_none() && is_self_server_command(&imported.command) {
            skipped_self_servers.push(name);
            continue;
        }

        importable_servers.push(ImportableServer {
            name,
            command: imported.command,
            url: imported.url,
            headers: imported.headers,
            enabled,
            env: imported.env,
            env_vars: imported.env_vars,
        });
    }

    Ok(ImportPlan {
        servers: importable_servers,
        skipped_self_servers,
    })
}

pub(crate) fn load_opencode_servers_for_import_from_path(
    path: &Path,
) -> Result<ImportPlan, Box<dyn Error>> {
    if !path.exists() {
        return Err(format!(
            "OpenCode config not found at {}",
            format_path_for_display(path)
        )
        .into());
    }

    let contents = fs::read_to_string(path)?;
    let config: serde_json::Value = serde_json::from_str(&contents)?;
    let servers = config
        .get("mcp")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            format!(
                "no `mcp` object found in OpenCode config {}",
                format_path_for_display(path)
            )
        })?;

    if servers.is_empty() {
        return Err(format!(
            "no MCP servers found in OpenCode config {}",
            format_path_for_display(path)
        )
        .into());
    }

    let mut names = servers.keys().cloned().collect::<Vec<_>>();
    names.sort();

    let mut importable_servers = Vec::new();
    let mut skipped_self_servers = Vec::new();

    for name in names {
        let server = servers[&name]
            .as_object()
            .ok_or_else(|| format!("OpenCode MCP server `{name}` must be an object"))?;
        validate_importable_opencode_server(&name, server)?;
        let enabled = parse_opencode_import_server_enabled(server, &name)?;
        let imported = opencode_imported_server_command(server, &name)?;

        if imported.url.is_none() && is_self_server_command(&imported.command) {
            skipped_self_servers.push(name);
            continue;
        }

        importable_servers.push(ImportableServer {
            name,
            command: imported.command,
            url: imported.url,
            headers: imported.headers,
            enabled,
            env: imported.env,
            env_vars: imported.env_vars,
        });
    }

    Ok(ImportPlan {
        servers: importable_servers,
        skipped_self_servers,
    })
}

pub(crate) fn load_claude_servers_for_import_from_path(
    path: &Path,
) -> Result<ImportPlan, Box<dyn Error>> {
    if !path.exists() {
        return Err(format!(
            "Claude Code config not found at {}",
            format_path_for_display(path)
        )
        .into());
    }

    let contents = fs::read_to_string(path)?;
    let config: serde_json::Value = serde_json::from_str(&contents)?;
    let servers = config
        .get("mcpServers")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            format!(
                "no `mcpServers` object found in Claude Code config {}",
                format_path_for_display(path)
            )
        })?;

    if servers.is_empty() {
        return Err(format!(
            "no MCP servers found in Claude Code config {}",
            format_path_for_display(path)
        )
        .into());
    }

    let mut names = servers.keys().cloned().collect::<Vec<_>>();
    names.sort();

    let mut importable_servers = Vec::new();
    let mut skipped_self_servers = Vec::new();

    for name in names {
        let server = servers[&name]
            .as_object()
            .ok_or_else(|| format!("Claude Code MCP server `{name}` must be an object"))?;
        validate_importable_claude_server(&name, server)?;
        let imported = claude_imported_server_command(server, &name)?;

        if imported.url.is_none() && is_self_server_command(&imported.command) {
            skipped_self_servers.push(name);
            continue;
        }

        importable_servers.push(ImportableServer {
            name,
            command: imported.command,
            url: imported.url,
            headers: imported.headers,
            enabled: true,
            env: imported.env,
            env_vars: imported.env_vars,
        });
    }

    Ok(ImportPlan {
        servers: importable_servers,
        skipped_self_servers,
    })
}

fn parse_codex_import_server_enabled(server: &Table, name: &str) -> Result<bool, Box<dyn Error>> {
    match server.get("enabled") {
        Some(Value::Boolean(enabled)) => Ok(*enabled),
        Some(_) => {
            Err(format!("Codex MCP server `{name}` has a non-boolean `enabled` field").into())
        }
        None => Ok(true),
    }
}

fn parse_opencode_import_server_enabled(
    server: &JsonMap<String, JsonValue>,
    name: &str,
) -> Result<bool, Box<dyn Error>> {
    match server.get("enabled") {
        Some(JsonValue::Bool(enabled)) => Ok(*enabled),
        Some(_) => {
            Err(format!("OpenCode MCP server `{name}` has a non-boolean `enabled` field").into())
        }
        None => Ok(true),
    }
}

fn codex_imported_server_command(
    server: &Table,
    name: &str,
) -> Result<ImportedServerDefinition, Box<dyn Error>> {
    let env = parse_toml_string_table(server.get("env"), "env", "Codex MCP server", name)?;
    let mut env_vars =
        parse_toml_string_array(server.get("env_vars"), "env_vars", "Codex MCP server", name)?;

    match (
        server.get("url"),
        server.get("command").and_then(Value::as_str),
    ) {
        (Some(_), Some(_)) => {
            Err(format!("Codex MCP server `{name}` cannot define both `url` and `command`").into())
        }
        (Some(Value::String(url)), None) => {
            let mut headers = parse_toml_string_table(
                server.get("http_headers"),
                "http_headers",
                "Codex MCP server",
                name,
            )?;
            let env_http_headers = parse_toml_string_table(
                server.get("env_http_headers"),
                "env_http_headers",
                "Codex MCP server",
                name,
            )?;
            for (header_name, env_var_name) in env_http_headers {
                headers.insert(header_name, format!("{{env:{env_var_name}}}"));
            }
            if let Some(Value::String(bearer_token_env_var)) = server.get("bearer_token_env_var") {
                headers.insert(
                    "Authorization".to_string(),
                    format!("Bearer {{env:{bearer_token_env_var}}}"),
                );
            } else if server.get("bearer_token_env_var").is_some() {
                return Err(format!(
                    "Codex MCP server `{name}` has a non-string `bearer_token_env_var` field"
                )
                .into());
            }
            merge_env_vars(&mut env_vars, collect_remote_header_env_vars(&headers));
            Ok(ImportedServerDefinition {
                command: Vec::new(),
                url: Some(url.to_string()),
                headers,
                env,
                env_vars,
            })
        }
        (Some(_), None) => {
            Err(format!("Codex MCP server `{name}` has a non-string `url` field").into())
        }
        (None, Some(command)) => {
            let args = match server.get("args") {
                None => Vec::new(),
                Some(Value::Array(items)) => items
                    .iter()
                    .map(|value| {
                        value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                            format!("Codex MCP server `{name}` contains a non-string arg")
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                Some(_) => {
                    return Err(
                        format!("Codex MCP server `{name}` has a non-array `args` field").into(),
                    );
                }
            };
            let mut raw_command = vec![command.to_string()];
            raw_command.extend(args);
            Ok(ImportedServerDefinition {
                command: raw_command,
                url: None,
                headers: BTreeMap::new(),
                env,
                env_vars,
            })
        }
        (None, None) => {
            Err(format!("Codex MCP server `{name}` is missing `command` or `url`").into())
        }
    }
}

fn opencode_imported_server_command(
    server: &JsonMap<String, JsonValue>,
    name: &str,
) -> Result<ImportedServerDefinition, Box<dyn Error>> {
    match server.get("type").and_then(JsonValue::as_str).unwrap_or("local") {
        "local" => {
            let command = server
                .get("command")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| format!("OpenCode MCP server `{name}` is missing `command`"))?;
            if command.is_empty() {
                return Err(
                    format!("OpenCode MCP server `{name}` has an empty `command` array").into(),
                );
            }

            let raw_command = command
                .iter()
                .map(|value| {
                    value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                        format!("OpenCode MCP server `{name}` contains a non-string command part")
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let env = parse_json_string_object(
                server.get("environment"),
                "environment",
                "OpenCode MCP server",
                name,
            )?;
            Ok(ImportedServerDefinition {
                command: raw_command,
                url: None,
                headers: BTreeMap::new(),
                env,
                env_vars: Vec::new(),
            })
        }
        "remote" => {
            let url = server
                .get("url")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| format!("OpenCode MCP server `{name}` is missing `url`"))?;
            let headers = parse_json_string_object(
                server.get("headers"),
                "headers",
                "OpenCode MCP server",
                name,
            )?;
            let env_vars = collect_remote_header_env_vars(&headers);
            Ok(ImportedServerDefinition {
                command: Vec::new(),
                url: Some(url.to_string()),
                headers,
                env: BTreeMap::new(),
                env_vars,
            })
        }
        other => Err(format!(
            "OpenCode MCP server `{name}` uses unsupported type `{other}`, only `local` and `remote` can be imported"
        )
        .into()),
    }
}

fn claude_imported_server_command(
    server: &JsonMap<String, JsonValue>,
    name: &str,
) -> Result<ImportedServerDefinition, Box<dyn Error>> {
    match server.get("type").and_then(JsonValue::as_str).unwrap_or("stdio") {
        "stdio" => {
            let command = server
                .get("command")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| format!("Claude Code MCP server `{name}` is missing `command`"))?;
            let args = match server.get("args") {
                None => Vec::new(),
                Some(JsonValue::Array(items)) => items
                    .iter()
                    .map(|value| {
                        value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                            format!("Claude Code MCP server `{name}` contains a non-string arg")
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                Some(_) => {
                    return Err(format!(
                        "Claude Code MCP server `{name}` has a non-array `args` field"
                    )
                    .into());
                }
            };
            let env =
                parse_json_string_object(server.get("env"), "env", "Claude Code MCP server", name)?;
            let mut raw_command = vec![command.to_string()];
            raw_command.extend(args);
            Ok(ImportedServerDefinition {
                command: raw_command,
                url: None,
                headers: BTreeMap::new(),
                env,
                env_vars: Vec::new(),
            })
        }
        "http" | "sse" => {
            let url = server
                .get("url")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| format!("Claude Code MCP server `{name}` is missing `url`"))?;
            let headers = parse_json_string_object(
                server.get("headers"),
                "headers",
                "Claude Code MCP server",
                name,
            )?;
            let env_vars = collect_remote_header_env_vars(&headers);
            Ok(ImportedServerDefinition {
                command: Vec::new(),
                url: Some(url.to_string()),
                headers,
                env: BTreeMap::new(),
                env_vars,
            })
        }
        other => Err(format!(
            "Claude Code MCP server `{name}` uses unsupported type `{other}`, only `stdio`, `http`, and `sse` can be imported"
        )
        .into()),
    }
}

fn collect_remote_header_env_vars(headers: &BTreeMap<String, String>) -> Vec<String> {
    let mut env_vars = Vec::new();

    for value in headers.values() {
        merge_env_vars(&mut env_vars, collect_remote_header_value_env_vars(value));
    }

    env_vars
}

pub(crate) fn collect_remote_header_value_env_vars(value: &str) -> Vec<String> {
    collect_env_var_names(value)
}

fn codex_server_value(server: &StdioServer) -> Value {
    let mut server_table = Table::new();
    server_table.insert("command".to_string(), Value::String(server.command.clone()));
    server_table.insert(
        "args".to_string(),
        Value::Array(server.args.iter().cloned().map(Value::String).collect()),
    );
    Value::Table(server_table)
}

fn opencode_server_value(server: &StdioServer) -> JsonValue {
    JsonValue::Object(JsonMap::from_iter([
        ("type".to_string(), JsonValue::String("local".to_string())),
        (
            "command".to_string(),
            JsonValue::Array(
                server
                    .raw_command()
                    .into_iter()
                    .map(JsonValue::String)
                    .collect(),
            ),
        ),
    ]))
}

fn claude_server_value(server: &StdioServer) -> JsonValue {
    JsonValue::Object(JsonMap::from_iter([
        ("type".to_string(), JsonValue::String("stdio".to_string())),
        (
            "command".to_string(),
            JsonValue::String(server.command.clone()),
        ),
        (
            "args".to_string(),
            JsonValue::Array(server.args.iter().cloned().map(JsonValue::String).collect()),
        ),
    ]))
}

pub(crate) fn load_opencode_config(path: &Path) -> Result<JsonValue, Box<dyn Error>> {
    if !path.exists() {
        return Ok(JsonValue::Object(JsonMap::new()));
    }

    let contents = fs::read_to_string(path)?;
    let value = serde_json::from_str(&contents)?;
    Ok(value)
}

fn save_opencode_config(path: &Path, config: &JsonValue) -> Result<(), Box<dyn Error>> {
    let contents = serde_json::to_string_pretty(config)?;
    write_file_atomically(path, contents.as_bytes())?;
    Ok(())
}

pub(crate) fn load_claude_config(path: &Path) -> Result<JsonValue, Box<dyn Error>> {
    if !path.exists() {
        return Ok(JsonValue::Object(JsonMap::new()));
    }

    let contents = fs::read_to_string(path)?;
    let value = serde_json::from_str(&contents)?;
    Ok(value)
}

fn save_claude_config(path: &Path, config: &JsonValue) -> Result<(), Box<dyn Error>> {
    let contents = serde_json::to_string_pretty(config)?;
    write_file_atomically(path, contents.as_bytes())?;
    Ok(())
}

fn merge_codex_servers_into_backup(
    backup_path: &Path,
    servers: &Table,
) -> Result<(), Box<dyn Error>> {
    let mut backup = load_config_table(backup_path)?;
    let backup_servers_value = backup
        .entry("mcp_servers")
        .or_insert_with(|| Value::Table(Table::new()));
    let backup_servers = backup_servers_value
        .as_table_mut()
        .ok_or_else(|| "`mcp_servers` in Codex backup must be a table".to_string())?;

    for (name, server) in servers {
        backup_servers.insert(name.clone(), server.clone());
    }

    save_config_table(backup_path, &backup)?;
    Ok(())
}

fn merge_opencode_servers_into_backup(
    backup_path: &Path,
    servers: &JsonMap<String, JsonValue>,
) -> Result<(), Box<dyn Error>> {
    let mut backup = load_opencode_config(backup_path)?;
    let root = backup
        .as_object_mut()
        .ok_or_else(|| "OpenCode backup root must be a JSON object".to_string())?;
    let backup_servers_value = root
        .entry("mcp".to_string())
        .or_insert_with(|| JsonValue::Object(JsonMap::new()));
    let backup_servers = backup_servers_value
        .as_object_mut()
        .ok_or_else(|| "`mcp` in OpenCode backup must be an object".to_string())?;

    for (name, server) in servers {
        backup_servers.insert(name.clone(), server.clone());
    }

    save_opencode_config(backup_path, &backup)?;
    Ok(())
}

fn merge_claude_servers_into_backup(
    backup_path: &Path,
    servers: &JsonMap<String, JsonValue>,
) -> Result<(), Box<dyn Error>> {
    let mut backup = load_claude_config(backup_path)?;
    let root = backup
        .as_object_mut()
        .ok_or_else(|| "Claude Code backup root must be a JSON object".to_string())?;
    let backup_servers_value = root
        .entry("mcpServers".to_string())
        .or_insert_with(|| JsonValue::Object(JsonMap::new()));
    let backup_servers = backup_servers_value
        .as_object_mut()
        .ok_or_else(|| "`mcpServers` in Claude Code backup must be an object".to_string())?;

    for (name, server) in servers {
        backup_servers.insert(name.clone(), server.clone());
    }

    save_claude_config(backup_path, &backup)?;
    Ok(())
}

fn merge_codex_servers_into_target(
    config: &mut Table,
    servers: &Table,
) -> Result<(), Box<dyn Error>> {
    let target_servers_value = config
        .entry("mcp_servers")
        .or_insert_with(|| Value::Table(Table::new()));
    let target_servers = target_servers_value
        .as_table_mut()
        .ok_or_else(|| "`mcp_servers` in Codex config must be a table".to_string())?;

    for (name, server) in servers {
        target_servers.insert(name.clone(), server.clone());
    }

    Ok(())
}

fn merge_opencode_servers_into_target(
    config: &mut JsonValue,
    servers: &JsonMap<String, JsonValue>,
) -> Result<(), Box<dyn Error>> {
    let root = config
        .as_object_mut()
        .ok_or_else(|| "OpenCode config root must be a JSON object".to_string())?;
    let target_servers_value = root
        .entry("mcp".to_string())
        .or_insert_with(|| JsonValue::Object(JsonMap::new()));
    let target_servers = target_servers_value
        .as_object_mut()
        .ok_or_else(|| "`mcp` in OpenCode config must be an object".to_string())?;

    for (name, server) in servers {
        target_servers.insert(name.clone(), server.clone());
    }

    Ok(())
}

fn merge_claude_servers_into_target(
    config: &mut JsonValue,
    servers: &JsonMap<String, JsonValue>,
) -> Result<(), Box<dyn Error>> {
    let root = config
        .as_object_mut()
        .ok_or_else(|| "Claude Code config root must be a JSON object".to_string())?;
    let target_servers_value = root
        .entry("mcpServers".to_string())
        .or_insert_with(|| JsonValue::Object(JsonMap::new()));
    let target_servers = target_servers_value
        .as_object_mut()
        .ok_or_else(|| "`mcpServers` in Claude Code config must be an object".to_string())?;

    for (name, server) in servers {
        target_servers.insert(name.clone(), server.clone());
    }

    Ok(())
}

fn load_required_codex_backup(path: &Path) -> Result<Table, Box<dyn Error>> {
    if !path.exists() {
        return Err(format!(
            "Codex backup not found at {}",
            format_path_for_display(path)
        )
        .into());
    }

    load_config_table(path)
}

fn load_required_opencode_backup(path: &Path) -> Result<JsonValue, Box<dyn Error>> {
    if !path.exists() {
        return Err(format!(
            "OpenCode backup not found at {}",
            format_path_for_display(path)
        )
        .into());
    }

    load_opencode_config(path)
}

fn load_required_claude_backup(path: &Path) -> Result<JsonValue, Box<dyn Error>> {
    if !path.exists() {
        return Err(format!(
            "Claude Code backup not found at {}",
            format_path_for_display(path)
        )
        .into());
    }

    load_claude_config(path)
}

fn remove_codex_self_servers(config: &mut Table) -> Result<usize, Box<dyn Error>> {
    let Some(servers_value) = config.get_mut("mcp_servers") else {
        return Ok(0);
    };
    let servers = servers_value
        .as_table_mut()
        .ok_or_else(|| "`mcp_servers` in Codex config must be a table".to_string())?;

    let names = servers
        .iter()
        .filter_map(|(name, value)| {
            let server = value.as_table()?;
            let raw_command = codex_server_raw_command(server)?;
            is_self_server_command(&raw_command).then_some(name.clone())
        })
        .collect::<Vec<_>>();

    for name in &names {
        servers.remove(name);
    }

    if servers.is_empty() {
        config.remove("mcp_servers");
    }

    Ok(names.len())
}

fn remove_opencode_self_servers(config: &mut JsonValue) -> Result<usize, Box<dyn Error>> {
    let root = config
        .as_object_mut()
        .ok_or_else(|| "OpenCode config root must be a JSON object".to_string())?;
    let Some(servers_value) = root.get_mut("mcp") else {
        return Ok(0);
    };
    let servers = servers_value
        .as_object_mut()
        .ok_or_else(|| "`mcp` in OpenCode config must be an object".to_string())?;

    let names = servers
        .iter()
        .filter_map(|(name, value)| {
            let server = value.as_object()?;
            let raw_command = opencode_server_raw_command(server)?;
            is_self_server_command(&raw_command).then_some(name.clone())
        })
        .collect::<Vec<_>>();

    for name in &names {
        servers.remove(name);
    }

    if servers.is_empty() {
        root.remove("mcp");
    }

    Ok(names.len())
}

fn remove_claude_self_servers(config: &mut JsonValue) -> Result<usize, Box<dyn Error>> {
    let Some(root) = config.as_object_mut() else {
        return Err("Claude Code config root must be a JSON object".into());
    };
    let Some(servers_value) = root.get_mut("mcpServers") else {
        return Ok(0);
    };
    let servers = servers_value
        .as_object_mut()
        .ok_or_else(|| "`mcpServers` in Claude Code config must be an object".to_string())?;

    let names = servers
        .iter()
        .filter_map(|(name, value)| {
            let server = value.as_object()?;
            let raw_command = claude_server_raw_command(server)?;
            is_self_server_command(&raw_command).then_some(name.clone())
        })
        .collect::<Vec<_>>();

    for name in &names {
        servers.remove(name);
    }

    if servers.is_empty() {
        root.remove("mcpServers");
    }

    Ok(names.len())
}

fn validate_importable_codex_server(name: &str, server: &Table) -> Result<(), Box<dyn Error>> {
    let unsupported_keys = server
        .keys()
        .filter(|key| {
            !matches!(
                key.as_str(),
                "command"
                    | "args"
                    | "enabled"
                    | "env"
                    | "env_vars"
                    | "url"
                    | "http_headers"
                    | "bearer_token_env_var"
                    | "env_http_headers"
            )
        })
        .map(|key| format!("`{key}`"))
        .collect::<Vec<_>>();

    if unsupported_keys.is_empty() {
        return Ok(());
    }

    Err(format!(
        "Codex MCP server `{name}` uses unsupported settings {}; only `command`, `args`, optional `enabled`, `env`, `env_vars`, or remote `url` with optional `http_headers`, `bearer_token_env_var`, and `env_http_headers` can be imported",
        unsupported_keys.join(", ")
    )
    .into())
}

fn validate_importable_opencode_server(
    name: &str,
    server: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), Box<dyn Error>> {
    let server_type = match server.get("type") {
        Some(JsonValue::String(value)) => value.as_str(),
        Some(_) => {
            return Err(
                format!("OpenCode MCP server `{name}` has a non-string `type` field").into(),
            );
        }
        None => "local",
    };

    let supported_keys = match server_type {
        "local" => ["command", "type", "enabled", "environment"].as_slice(),
        "remote" => ["url", "type", "enabled", "headers"].as_slice(),
        other => {
            return Err(format!(
                "OpenCode MCP server `{name}` uses unsupported type `{other}`, only `local` and `remote` can be imported"
            )
            .into());
        }
    };

    let unsupported_keys = server
        .keys()
        .filter(|key| !supported_keys.contains(&key.as_str()))
        .map(|key| format!("`{key}`"))
        .collect::<Vec<_>>();

    if unsupported_keys.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "OpenCode MCP server `{name}` uses unsupported settings {}; only {} can be imported",
            unsupported_keys.join(", "),
            match server_type {
                "local" => "`command` and optional `type`, `enabled`, and `environment`",
                "remote" => "`url` and optional `type`, `enabled`, and `headers`",
                _ => unreachable!(),
            }
        )
        .into())
    }
}

fn validate_importable_claude_server(
    name: &str,
    server: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), Box<dyn Error>> {
    let server_type = match server.get("type") {
        Some(JsonValue::String(value)) => value.as_str(),
        Some(_) => {
            return Err(
                format!("Claude Code MCP server `{name}` has a non-string `type` field").into(),
            );
        }
        None => "stdio",
    };

    let supported_keys = match server_type {
        "stdio" => ["command", "args", "env", "type"].as_slice(),
        "http" | "sse" => ["url", "headers", "type"].as_slice(),
        other => {
            return Err(format!(
                "Claude Code MCP server `{name}` uses unsupported type `{other}`, only `stdio`, `http`, and `sse` can be imported"
            )
            .into());
        }
    };

    let unsupported_keys = server
        .keys()
        .filter(|key| !supported_keys.contains(&key.as_str()))
        .map(|key| format!("`{key}`"))
        .collect::<Vec<_>>();

    if unsupported_keys.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Claude Code MCP server `{name}` uses unsupported settings {}; only {} can be imported",
            unsupported_keys.join(", "),
            match server_type {
                "stdio" => "`command`, optional `args`, optional `env`, and optional `type`",
                "http" | "sse" => "`url`, optional `headers`, and optional `type`",
                _ => unreachable!(),
            }
        )
        .into())
    }
}
