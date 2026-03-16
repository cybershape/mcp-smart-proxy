use std::env;
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
use tokio::io::AsyncWriteExt;

use crate::config::{configured_server, load_config_table, load_default_model_provider_config};
use crate::paths::{cache_file_path, unix_epoch_ms};
use crate::types::{
    CachedTools, CodexRuntimeConfig, ConfiguredServer, ModelProviderConfig, OpenAiRuntimeConfig,
    tool_snapshot,
};

pub async fn reload_server(config_path: &Path, name: &str) -> Result<PathBuf, Box<dyn Error>> {
    let config = load_config_table(config_path)?;
    let provider = load_default_model_provider_config(&config)?;
    let (resolved_name, server) = configured_server(&config, name)?;
    let tools = fetch_tools(&server).await?;
    let summary = summarize_tools(&provider, &resolved_name, &tools).await?;
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
    provider: &ModelProviderConfig,
    server_name: &str,
    tools: &[Tool],
) -> Result<String, Box<dyn Error>> {
    let prompt = build_summary_prompt(server_name, tools)?;

    match provider {
        ModelProviderConfig::OpenAi(openai) => summarize_tools_with_openai(openai, &prompt).await,
        ModelProviderConfig::Codex(codex) => summarize_tools_with_codex(codex, &prompt).await,
    }
}

fn build_summary_prompt(server_name: &str, tools: &[Tool]) -> Result<String, Box<dyn Error>> {
    let tools_json = serde_json::to_string_pretty(&tools)?;

    Ok(format!(
        "You are summarizing an MCP toolset for another AI.\n\
Server name: {server_name}\n\
Return exactly one concise English sentence.\n\
The sentence must explain when this toolset should be activated, based on the available tools.\n\
Do not mention implementation details like MCP, JSON, schema, or caching unless essential.\n\
Do not run shell commands or inspect the workspace. Use only the tool data provided below.\n\
If the tools cover multiple related workflows, summarize the common decision boundary.\n\n\
Tools:\n{tools_json}"
    ))
}

async fn summarize_tools_with_openai(
    openai: &OpenAiRuntimeConfig,
    prompt: &str,
) -> Result<String, Box<dyn Error>> {
    let mut model_builder = OpenAI::<DynamicModel>::builder()
        .model_name(openai.model.clone())
        .api_key(openai.key.clone());
    if let Some(baseurl) = &openai.baseurl {
        model_builder = model_builder.base_url(baseurl.clone());
    }
    let model = model_builder.build()?;

    let mut request = LanguageModelRequest::builder()
        .model(model)
        .prompt(prompt.to_string())
        .build();
    let response = request.generate_text().await?;
    non_empty_summary(
        response.text().as_deref(),
        "OpenAI returned an empty summary",
    )
}

async fn summarize_tools_with_codex(
    codex: &CodexRuntimeConfig,
    prompt: &str,
) -> Result<String, Box<dyn Error>> {
    let workdir = codex_workdir_path()?;
    fs::create_dir(&workdir)?;
    let output_path = codex_output_path()?;
    let mut child = tokio::process::Command::new("codex");
    child
        .current_dir(&workdir)
        .arg("exec")
        .arg("--model")
        .arg(&codex.model)
        .arg("--skip-git-repo-check")
        .arg("--sandbox")
        .arg("read-only")
        .arg("--output-last-message")
        .arg(&output_path)
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());

    let mut child = child.spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "failed to open stdin for `codex exec`".to_string())?;
    stdin.write_all(prompt.as_bytes()).await?;
    drop(stdin);

    let status = child.wait().await?;
    if !status.success() {
        let _ = fs::remove_file(&output_path);
        let _ = fs::remove_dir(&workdir);
        return Err(format!("`codex exec` failed with status {status}").into());
    }

    let output = fs::read_to_string(&output_path)?;
    let _ = fs::remove_file(&output_path);
    let _ = fs::remove_dir(&workdir);
    non_empty_summary(Some(output.as_str()), "Codex returned an empty summary")
}

fn codex_output_path() -> Result<PathBuf, Box<dyn Error>> {
    Ok(env::temp_dir().join(format!(
        "mcp-smart-proxy-codex-summary-{}-{}.txt",
        std::process::id(),
        unix_epoch_ms()?
    )))
}

fn codex_workdir_path() -> Result<PathBuf, Box<dyn Error>> {
    Ok(env::temp_dir().join(format!(
        "mcp-smart-proxy-codex-workdir-{}-{}",
        std::process::id(),
        unix_epoch_ms()?
    )))
}

fn non_empty_summary(value: Option<&str>, empty_message: &str) -> Result<String, Box<dyn Error>> {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| empty_message.to_string().into())
}

fn write_cache(path: &Path, payload: &CachedTools) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let contents = serde_json::to_string_pretty(payload)?;
    fs::write(path, contents)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_output_path_is_created_in_temp_dir() {
        let path = codex_output_path().unwrap();

        assert!(path.starts_with(env::temp_dir()));
        assert!(
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap()
                .starts_with("mcp-smart-proxy-codex-summary-")
        );
    }

    #[test]
    fn codex_workdir_path_is_created_in_temp_dir() {
        let path = codex_workdir_path().unwrap();

        assert!(path.starts_with(env::temp_dir()));
        assert!(
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap()
                .starts_with("mcp-smart-proxy-codex-workdir-")
        );
    }

    #[test]
    fn rejects_empty_summary_text() {
        let error = non_empty_summary(Some("   "), "empty").unwrap_err();

        assert_eq!(error.to_string(), "empty");
    }
}
