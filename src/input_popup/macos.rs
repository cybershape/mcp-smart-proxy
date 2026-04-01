use std::error::Error;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::console::operation_error;
use crate::fs_util::{acquire_sibling_lock, write_file_atomically};
use crate::paths::{cache_dir_path_from_home, home_dir};

use super::schema::{PopupInputRequest, PopupInputResponse};

const POPUP_HELPER_BYTES: &[u8] = include_bytes!(env!("MSP_POPUP_HELPER_BINARY"));
const POPUP_HELPER_DIR: &str = "popup-input";
const POPUP_HELPER_NAME_PREFIX: &str = "msp-popup-input-helper";

pub fn show_popup_dialog(request: PopupInputRequest) -> Result<PopupInputResponse, Box<dyn Error>> {
    let request = request.normalized();
    if request.questions.is_empty() {
        return Ok(PopupInputResponse::cancelled());
    }
    let helper_path = ensure_popup_helper_installed()?;
    run_popup_helper(&helper_path, &request)
}

fn ensure_popup_helper_installed() -> Result<PathBuf, Box<dyn Error>> {
    let cache_dir = cache_dir_path_from_home(&home_dir()?).map_err(|error| {
        operation_error(
            "input.popup.helper.cache_dir",
            "failed to resolve the popup helper cache directory",
            error,
        )
    })?;
    let helper_dir = cache_dir.join(POPUP_HELPER_DIR);
    fs::create_dir_all(&helper_dir).map_err(|error| {
        operation_error(
            "input.popup.helper.create_dir",
            format!(
                "failed to create the popup helper cache directory at {}",
                helper_dir.display()
            ),
            Box::new(error),
        )
    })?;

    let helper_path = helper_dir.join(helper_file_name());
    let _lock = acquire_sibling_lock(&helper_path).map_err(|error| {
        operation_error(
            "input.popup.helper.lock",
            format!(
                "failed to lock the popup helper path at {}",
                helper_path.display()
            ),
            Box::new(error),
        )
    })?;

    if helper_needs_refresh(&helper_path)? {
        write_file_atomically(&helper_path, POPUP_HELPER_BYTES).map_err(|error| {
            operation_error(
                "input.popup.helper.write",
                format!(
                    "failed to install the popup helper binary at {}",
                    helper_path.display()
                ),
                Box::new(error),
            )
        })?;
        fs::set_permissions(&helper_path, fs::Permissions::from_mode(0o755)).map_err(|error| {
            operation_error(
                "input.popup.helper.permissions",
                format!(
                    "failed to mark the popup helper binary as executable at {}",
                    helper_path.display()
                ),
                Box::new(error),
            )
        })?;
    }

    cleanup_stale_helpers(&helper_dir, &helper_path);
    Ok(helper_path)
}

fn helper_needs_refresh(path: &Path) -> Result<bool, Box<dyn Error>> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(error) => {
            return Err(operation_error(
                "input.popup.helper.metadata",
                format!(
                    "failed to inspect the popup helper binary at {}",
                    path.display()
                ),
                Box::new(error),
            ));
        }
    };

    Ok(metadata.len() != POPUP_HELPER_BYTES.len() as u64
        || metadata.permissions().mode() & 0o111 == 0)
}

fn run_popup_helper(
    helper_path: &Path,
    request: &PopupInputRequest,
) -> Result<PopupInputResponse, Box<dyn Error>> {
    let payload = serde_json::to_vec(request).map_err(|error| {
        operation_error(
            "input.popup.helper.serialize",
            "failed to encode popup input request as JSON",
            Box::new(error),
        )
    })?;
    let mut child = Command::new(helper_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            operation_error(
                "input.popup.helper.spawn",
                format!(
                    "failed to start the popup helper executable at {}",
                    helper_path.display()
                ),
                Box::new(error),
            )
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(&payload).map_err(|error| {
            operation_error(
                "input.popup.helper.write_stdin",
                "failed to send the popup input request to the helper",
                Box::new(error),
            )
        })?;
    }

    let output = child.wait_with_output().map_err(|error| {
        operation_error(
            "input.popup.helper.wait",
            "failed to wait for the popup helper process",
            Box::new(error),
        )
    })?;

    if !output.status.success() {
        return Err(operation_error(
            "input.popup.helper.status",
            format!(
                "popup helper exited with status {} and stderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            format!("popup helper status {}", output.status).into(),
        ));
    }

    serde_json::from_slice(&output.stdout).map_err(|error| {
        operation_error(
            "input.popup.helper.parse_response",
            "failed to decode the popup helper JSON response",
            Box::new(error),
        )
    })
}

fn helper_file_name() -> String {
    format!("{POPUP_HELPER_NAME_PREFIX}-{:016x}", helper_hash())
}

fn helper_hash() -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in POPUP_HELPER_BYTES {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn cleanup_stale_helpers(helper_dir: &Path, current_path: &Path) {
    let entries = match fs::read_dir(helper_dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path == current_path {
            continue;
        }

        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !file_name.starts_with(POPUP_HELPER_NAME_PREFIX) {
            continue;
        }

        let _ = fs::remove_file(path);
    }
}
