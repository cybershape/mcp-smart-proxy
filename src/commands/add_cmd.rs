use std::error::Error;
use std::path::Path;

use crate::cli::ProviderName;
use crate::config::{AddServerConfig, add_server_with_config, remove_server};
use crate::console::{operation_error, print_app_event};
use crate::paths::format_path_for_display;
use crate::reload::reload_server_with_provider;

use super::config_cmd::parse_key_value_assignments;
use super::provider::resolve_default_command_provider;

pub(crate) struct AddCommandArgs {
    pub(crate) url: Option<String>,
    pub(crate) enabled: Option<bool>,
    pub(crate) headers: Vec<String>,
    pub(crate) env: Vec<String>,
    pub(crate) env_vars: Vec<String>,
    pub(crate) command: Vec<String>,
}

impl AddCommandArgs {
    fn into_add_server_config(self, server_name: &str) -> Result<AddServerConfig, Box<dyn Error>> {
        let headers = parse_key_value_assignments(&self.headers, "header").map_err(|error| {
            format!("failed to parse `--header` values for server `{server_name}`: {error}")
        })?;
        let env = parse_key_value_assignments(&self.env, "env").map_err(|error| {
            format!("failed to parse `--env` values for server `{server_name}`: {error}")
        })?;

        Ok(AddServerConfig {
            command: self.command,
            url: self.url,
            headers,
            enabled: self.enabled.unwrap_or(true),
            env,
            env_vars: dedupe_env_vars(self.env_vars),
        })
    }
}

pub(crate) async fn run_add_command(
    config_path: &Path,
    provider: ProviderName,
    name: &str,
    args: AddCommandArgs,
) -> Result<(), Box<dyn Error>> {
    let resolved_provider = resolve_default_command_provider(Some(provider)).map_err(|error| {
        operation_error(
            "cli.add.load_provider",
            format!("failed to resolve the summary provider before adding `{name}`"),
            error,
        )
    })?;
    let add_config = args.into_add_server_config(name).map_err(|error| {
        operation_error(
            "cli.add.args",
            format!("failed to parse config values for MCP server `{name}`"),
            error,
        )
    })?;
    let server_name = add_server_with_config(config_path, name, add_config).map_err(|error| {
        operation_error(
            "cli.add",
            format!(
                "failed to add MCP server `{name}` into {}",
                format_path_for_display(config_path)
            ),
            error,
        )
    })?;
    let reload_result = match reload_server_with_provider(
        config_path,
        &server_name,
        &resolved_provider,
    )
    .await
    {
        Ok(result) => result,
        Err(error) => {
            remove_server(config_path, &server_name).map_err(|rollback_error| {
                    operation_error(
                        "cli.add.rollback",
                        format!(
                            "failed to roll back MCP server `{server_name}` in {} after cache generation failed: original cache error: {}",
                            format_path_for_display(config_path),
                            error
                        ),
                        rollback_error,
                    )
                })?;
            return Err(operation_error(
                "cli.add.reload",
                format!(
                    "failed to populate the initial cache for MCP server `{server_name}`; rolled back the config change in {}",
                    format_path_for_display(config_path)
                ),
                error,
            ));
        }
    };
    print_app_event(
        "cli.add",
        if reload_result.updated {
            format!(
                "Added MCP server `{server_name}` to {} and cached its tools at {}",
                format_path_for_display(config_path),
                format_path_for_display(&reload_result.cache_path)
            )
        } else {
            format!(
                "Added MCP server `{server_name}` to {} and reused matching cached tools at {}",
                format_path_for_display(config_path),
                format_path_for_display(&reload_result.cache_path)
            )
        },
    );

    Ok(())
}

fn dedupe_env_vars(env_vars: Vec<String>) -> Vec<String> {
    let mut deduped = Vec::new();
    for name in env_vars {
        if !deduped.contains(&name) {
            deduped.push(name);
        }
    }
    deduped
}
