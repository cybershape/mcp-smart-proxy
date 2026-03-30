use std::error::Error;
use std::ffi::OsString;
use std::process::Stdio;

use rmcp::{
    RoleClient, ServiceExt,
    service::RunningService,
    transport::{ConfigureCommandExt, TokioChildProcess},
};

use crate::console::{
    ExternalOutputRouter, describe_command, operation_error,
    print_external_command_failure_with_captured_stderr, spawn_stderr_collector,
};

pub struct DownstreamStdioClient {
    pub service: RunningService<RoleClient, ()>,
    pub stderr: ExternalOutputRouter,
    pub command_line: String,
    pub label: String,
}

pub async fn connect_stdio_client(
    failure_stage: &'static str,
    spawn_stage: &'static str,
    connect_stage: &'static str,
    label: String,
    command: &str,
    args: &[String],
    env: Vec<(String, OsString)>,
) -> Result<DownstreamStdioClient, Box<dyn Error>> {
    let command_line = describe_command(command, args);
    let stderr = ExternalOutputRouter::new();
    let stderr_capture = stderr.start_capture().await;
    let (transport, child_stderr) =
        TokioChildProcess::builder(tokio::process::Command::new(command).configure(move |cmd| {
            cmd.args(args);
            for (name, value) in env {
                cmd.env(name, value);
            }
        }))
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            operation_error(
                spawn_stage,
                format!("failed to start external command `{command_line}`"),
                Box::new(error),
            )
        })?;

    if let Some(child_stderr) = child_stderr {
        spawn_stderr_collector(
            failure_stage.to_string(),
            label.clone(),
            command_line.clone(),
            child_stderr,
            stderr.clone(),
        );
    }

    let service = match ().serve(transport).await {
        Ok(service) => service,
        Err(error) => {
            let stderr_content = stderr_capture.finish().await;
            print_external_command_failure_with_captured_stderr(
                failure_stage,
                &label,
                &command_line,
                "connect-failed",
                &stderr_content,
            )
            .await;
            return Err(operation_error(
                connect_stage,
                format!(
                    "failed to initialize an MCP client against external command `{command_line}`"
                ),
                Box::new(error),
            ));
        }
    };
    let _ = stderr_capture.finish().await;

    Ok(DownstreamStdioClient {
        service,
        stderr,
        command_line,
        label,
    })
}
