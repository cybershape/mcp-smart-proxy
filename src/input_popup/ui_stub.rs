use std::error::Error;

use crate::console::message_error;

use super::{POPUP_INPUT_UNSUPPORTED_MESSAGE, PopupInputRequest, PopupInputResponse};

pub fn show_popup_dialog(
    _request: PopupInputRequest,
) -> Result<PopupInputResponse, Box<dyn Error>> {
    Err(message_error(
        "input.popup.unsupported",
        POPUP_INPUT_UNSUPPORTED_MESSAGE,
    ))
}
