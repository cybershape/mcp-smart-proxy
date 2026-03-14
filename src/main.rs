use std::error::Error;

use clap::Parser;

mod cli;
mod config;
mod mcp_server;
mod paths;
mod reload;
mod types;

use cli::{Cli, Command, ConfigCommand};
use config::{OpenAiConfigUpdate, add_server, load_config_table, update_openai_config};
use paths::expand_tilde;
use reload::reload_server;

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
            if config_path.exists() {
                let _ = load_config_table(&config_path)?;
            }
        }
    }

    Ok(())
}
