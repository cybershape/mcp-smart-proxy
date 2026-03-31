use std::error::Error;
use std::path::PathBuf;
use std::process::Stdio;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::console::operation_error;

use super::schema::{PopupInputRequest, PopupInputResponse};

pub async fn request_user_input_in_popup(
    request: &PopupInputRequest,
) -> Result<PopupInputResponse, Box<dyn Error>> {
    let current_executable = std::env::current_exe().map_err(|error| {
        operation_error(
            "input.popup.current_exe",
            "failed to locate the current msp executable for popup input",
            Box::new(error),
        )
    })?;
    run_popup_subprocess(&current_executable, request).await
}

async fn run_popup_subprocess(
    executable: &PathBuf,
    request: &PopupInputRequest,
) -> Result<PopupInputResponse, Box<dyn Error>> {
    let payload = serde_json::to_vec(request).map_err(|error| {
        operation_error(
            "input.popup.serialize",
            "failed to encode popup input request as JSON",
            Box::new(error),
        )
    })?;
    let mut child = Command::new(executable)
        .arg("input")
        .arg("popup")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            operation_error(
                "input.popup.spawn",
                "failed to start the popup input subprocess",
                Box::new(error),
            )
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(&payload).await.map_err(|error| {
            operation_error(
                "input.popup.write_stdin",
                "failed to send popup input request to the popup subprocess",
                Box::new(error),
            )
        })?;
    }

    let output = child.wait_with_output().await.map_err(|error| {
        operation_error(
            "input.popup.wait",
            "failed to wait for the popup input subprocess",
            Box::new(error),
        )
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(operation_error(
            "input.popup.status",
            format!(
                "popup input subprocess exited with status {} and stderr:\n{}",
                output.status,
                stderr.trim()
            ),
            format!("popup subprocess status {}", output.status).into(),
        ));
    }

    serde_json::from_slice::<PopupInputResponse>(&output.stdout).map_err(|error| {
        operation_error(
            "input.popup.parse_response",
            "failed to decode popup input subprocess JSON response",
            Box::new(error),
        )
    })
}
