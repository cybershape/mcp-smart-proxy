use std::error::Error;
use std::sync::{Arc, Mutex};

use iced::widget::{button, column, container, radio, row, scrollable, text, text_input};
use iced::{
    Alignment, Background, Border, Color, Element, Length, Shadow, Subscription, Task, Theme,
    keyboard,
};

use super::schema::{PopupInputRequest, PopupInputResponse, is_other_label};

const WINDOW_WIDTH: f32 = 980.0;
const WINDOW_HEIGHT: f32 = 720.0;
const CARD_MAX_WIDTH: f32 = 920.0;

pub fn show_popup_dialog(request: PopupInputRequest) -> Result<PopupInputResponse, Box<dyn Error>> {
    let request = request.normalized();
    let shared_response = Arc::new(Mutex::new(None));
    let shared_response_for_boot = Arc::clone(&shared_response);

    iced::application(title, update, view)
        .theme(|_| Theme::Light)
        .subscription(subscription)
        .centered()
        .window_size([WINDOW_WIDTH, WINDOW_HEIGHT])
        .run_with(move || {
            (
                PopupApp::new(request, shared_response_for_boot),
                Task::none(),
            )
        })
        .map_err(|error| -> Box<dyn Error> { Box::new(error) })?;

    Ok(shared_response
        .lock()
        .map_err(|error| format!("failed to read popup result: {error}"))?
        .clone()
        .unwrap_or_else(PopupInputResponse::cancelled))
}

#[derive(Debug)]
struct PopupApp {
    request: PopupInputRequest,
    questions: Vec<QuestionState>,
    shared_response: Arc<Mutex<Option<PopupInputResponse>>>,
}

#[derive(Debug, Clone)]
struct QuestionState {
    selected_option: Option<usize>,
    custom_value: String,
}

#[derive(Debug, Clone)]
enum Message {
    SelectOption {
        question_index: usize,
        option_index: usize,
    },
    UpdateCustomValue {
        question_index: usize,
        value: String,
    },
    Submit,
    Cancel,
}

impl PopupApp {
    fn new(
        request: PopupInputRequest,
        shared_response: Arc<Mutex<Option<PopupInputResponse>>>,
    ) -> Self {
        let questions = request
            .questions
            .iter()
            .map(|_| QuestionState {
                selected_option: None,
                custom_value: String::new(),
            })
            .collect();

        Self {
            request,
            questions,
            shared_response,
        }
    }

    fn is_complete(&self) -> bool {
        self.questions
            .iter()
            .zip(self.request.questions.iter())
            .all(|(state, question)| match state.selected_option {
                Some(option_index) => {
                    let option = &question.options[option_index];
                    if is_other_label(&option.label) {
                        !state.custom_value.trim().is_empty()
                    } else {
                        true
                    }
                }
                None => false,
            })
    }

    fn response(&self) -> PopupInputResponse {
        PopupInputResponse::from_answers(
            self.questions
                .iter()
                .zip(self.request.questions.iter())
                .filter_map(|(state, question)| {
                    let option_index = state.selected_option?;
                    let option = &question.options[option_index];
                    let answer = if is_other_label(&option.label) {
                        let trimmed = state.custom_value.trim();
                        if trimmed.is_empty() {
                            return None;
                        }
                        trimmed.to_string()
                    } else {
                        option.label.clone()
                    };
                    Some((question.id.clone(), answer))
                }),
        )
    }

    fn set_shared_response(&self, response: PopupInputResponse) -> Task<Message> {
        if let Ok(mut slot) = self.shared_response.lock() {
            *slot = Some(response);
        }

        iced::exit()
    }
}

fn title(_state: &PopupApp) -> String {
    "MSP Input Request".to_string()
}

fn subscription(_state: &PopupApp) -> Subscription<Message> {
    keyboard::on_key_press(handle_key_press)
}

fn handle_key_press(key: keyboard::Key, _modifiers: keyboard::Modifiers) -> Option<Message> {
    match key.as_ref() {
        keyboard::Key::Named(keyboard::key::Named::Escape) => Some(Message::Cancel),
        _ => None,
    }
}

fn update(state: &mut PopupApp, message: Message) -> Task<Message> {
    match message {
        Message::SelectOption {
            question_index,
            option_index,
        } => {
            state.questions[question_index].selected_option = Some(option_index);
            Task::none()
        }
        Message::UpdateCustomValue {
            question_index,
            value,
        } => {
            state.questions[question_index].custom_value = value;
            Task::none()
        }
        Message::Submit => {
            if state.is_complete() {
                state.set_shared_response(state.response())
            } else {
                Task::none()
            }
        }
        Message::Cancel => state.set_shared_response(PopupInputResponse::cancelled()),
    }
}

fn view(state: &PopupApp) -> Element<'_, Message> {
    let title = text("Request user input")
        .size(36)
        .color(Color::from_rgb8(0x3d, 0x44, 0x63));
    let subtitle = text(
        "Choose one option for each question. Use the final Other option when you need a custom answer.",
    )
    .size(18)
    .color(Color::from_rgb8(0x72, 0x79, 0x97));

    let mut content = column![title, subtitle].spacing(18);

    for (question_index, (question, question_state)) in state
        .request
        .questions
        .iter()
        .zip(state.questions.iter())
        .enumerate()
    {
        content = content.push(question_card(question_index, question, question_state));
    }

    let dismiss_button = button(text("Cancel").size(22))
        .padding([16, 24])
        .style(button::secondary)
        .on_press(Message::Cancel);

    let submit_button = {
        let button = button(text("Submit").size(22))
            .padding([16, 28])
            .style(button::primary);
        if state.is_complete() {
            button.on_press(Message::Submit)
        } else {
            button
        }
    };

    let actions = row![dismiss_button, submit_button]
        .spacing(16)
        .align_y(Alignment::Center);

    let card = container(
        column![
            scrollable(content.spacing(24)).height(Length::Fill),
            actions
        ]
        .spacing(28),
    )
    .width(Length::Fill)
    .max_width(CARD_MAX_WIDTH)
    .padding(28)
    .style(card_style);

    container(card)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .padding(24)
        .style(backdrop_style)
        .into()
}

fn question_card<'a>(
    question_index: usize,
    question: &'a super::schema::PopupQuestion,
    question_state: &'a QuestionState,
) -> Element<'a, Message> {
    let header = text(&question.header)
        .size(16)
        .color(Color::from_rgb8(0x89, 0x90, 0xae));
    let prompt = text(&question.question)
        .size(28)
        .color(Color::from_rgb8(0x3d, 0x44, 0x63));

    let mut options = column![header, prompt].spacing(14);

    for (option_index, option) in question.options.iter().enumerate() {
        let description = text(&option.description)
            .size(16)
            .color(Color::from_rgb8(0x80, 0x87, 0xa3));

        let option_card = container(
            column![
                radio(
                    option.label.clone(),
                    option_index,
                    question_state.selected_option,
                    move |selected| Message::SelectOption {
                        question_index,
                        option_index: selected,
                    },
                )
                .size(24)
                .text_size(24),
                container(description).padding([0, 34]),
            ]
            .spacing(10),
        )
        .padding(18)
        .style(if question_state.selected_option == Some(option_index) {
            selected_option_style
        } else {
            option_style
        });

        options = options.push(option_card);

        if is_other_label(&option.label) && question_state.selected_option == Some(option_index) {
            let input = text_input("Enter a custom answer", &question_state.custom_value)
                .size(20)
                .padding(16)
                .on_input(move |value| Message::UpdateCustomValue {
                    question_index,
                    value,
                });

            options = options.push(container(input).padding([0, 12]).style(option_input_style));
        }
    }

    container(options.spacing(14))
        .padding(22)
        .style(question_style)
        .into()
}

fn backdrop_style(_theme: &Theme) -> container::Style {
    container::Style::default()
        .background(Background::Color(Color::from_rgba(0.95, 0.96, 0.99, 0.98)))
}

fn card_style(_theme: &Theme) -> container::Style {
    container::Style::default()
        .background(Background::Color(Color::WHITE))
        .border(Border {
            radius: 28.0.into(),
            width: 1.0,
            color: Color::from_rgb8(0xe4, 0xe7, 0xf0),
        })
        .shadow(Shadow {
            color: Color::from_rgba8(0x25, 0x2d, 0x4d, 0.12),
            offset: iced::Vector::new(0.0, 18.0),
            blur_radius: 48.0,
        })
}

fn question_style(_theme: &Theme) -> container::Style {
    container::Style::default()
        .background(Background::Color(Color::from_rgb8(0xfb, 0xfb, 0xfe)))
        .border(Border {
            radius: 22.0.into(),
            width: 1.0,
            color: Color::from_rgb8(0xea, 0xec, 0xf4),
        })
}

fn option_style(_theme: &Theme) -> container::Style {
    container::Style::default()
        .background(Background::Color(Color::from_rgb8(0xf4, 0xf5, 0xf9)))
        .border(Border {
            radius: 18.0.into(),
            width: 1.0,
            color: Color::from_rgb8(0xea, 0xec, 0xf4),
        })
}

fn selected_option_style(_theme: &Theme) -> container::Style {
    container::Style::default()
        .background(Background::Color(Color::from_rgb8(0xee, 0xf0, 0xf8)))
        .border(Border {
            radius: 18.0.into(),
            width: 1.0,
            color: Color::from_rgb8(0xb7, 0xc0, 0xe0),
        })
}

fn option_input_style(_theme: &Theme) -> container::Style {
    container::Style::default()
        .background(Background::Color(Color::from_rgb8(0xff, 0xff, 0xff)))
        .border(Border {
            radius: 16.0.into(),
            width: 1.0,
            color: Color::from_rgb8(0xd4, 0xd8, 0xe8),
        })
}
