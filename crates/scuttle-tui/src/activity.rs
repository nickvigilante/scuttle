//! The animated line that shows what the agent is doing.

use std::time::Duration;

use ratatui::text::{Line, Span};
use scuttle_core::app::Activity;

use crate::theme::Theme;

/// How often the spinner advances, and so how often the screen redraws while the agent works.
pub const SPINNER_INTERVAL: Duration = Duration::from_millis(100);

const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The spinner frame for `elapsed` time since the app started.
pub fn spinner_frame(elapsed: Duration) -> &'static str {
    let step = elapsed.as_millis() / SPINNER_INTERVAL.as_millis();
    FRAMES[(step % FRAMES.len() as u128) as usize]
}

pub fn label(activity: &Activity) -> String {
    match activity {
        Activity::Waiting => "Waiting for the agent…".into(),
        Activity::Thinking => "Thinking…".into(),
        Activity::Tool(name) if name.is_empty() => "Running a tool…".into(),
        Activity::Tool(name) => format!("Running {name}…"),
        Activity::Writing => "Writing…".into(),
        Activity::Interrupting => "Interrupting…".into(),
        Activity::Working => "Working…".into(),
    }
}

pub fn activity_line(activity: &Activity, elapsed: Duration, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{} ", spinner_frame(elapsed)), theme.accent),
        Span::styled(label(activity), theme.dim),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_advance_every_interval_and_wrap() {
        assert_ne!(
            spinner_frame(Duration::ZERO),
            spinner_frame(SPINNER_INTERVAL)
        );
        assert_eq!(
            spinner_frame(Duration::ZERO),
            spinner_frame(SPINNER_INTERVAL * 10)
        );
    }

    #[test]
    fn labels_name_the_tool() {
        assert_eq!(label(&Activity::Tool("execute".into())), "Running execute…");
        assert_eq!(label(&Activity::Tool(String::new())), "Running a tool…");
        assert_eq!(label(&Activity::Waiting), "Waiting for the agent…");
    }
}
