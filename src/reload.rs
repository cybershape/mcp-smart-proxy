use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use aisdk::{
    core::{DynamicModel, LanguageModelRequest},
    providers::OpenAI,
};
use rmcp::{
    ServiceExt,
    model::Tool,
    transport::{ConfigureCommandExt, TokioChildProcess},
};

use crate::config::{configured_server, load_config_table, load_openai_runtime_config};
use crate::paths::{cache_file_path, unix_epoch_ms};
use crate::types::{CachedTools, ConfiguredServer, OpenAiRuntimeConfig, tool_snapshot};

pub async fn reload_server(config_path: &Path, name: &str) -> Result<PathBuf, Box<dyn Error>> {
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

fn write_cache(path: &Path, payload: &CachedTools) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let contents = serde_json::to_string_pretty(payload)?;
    fs::write(path, contents)?;
    Ok(())
}
