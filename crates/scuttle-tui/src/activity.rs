//! The animated line that shows what the agent is doing, and the spinner styles it uses.

use std::ops::Range;
use std::time::Duration;

use ratatui::text::{Line, Span};
use scuttle_core::app::Activity;
use scuttle_core::config::SpinnerSetting;

use crate::theme::Theme;

/// How often the spinner advances, and so how often the screen redraws while the agent works.
pub const SPINNER_INTERVAL: Duration = Duration::from_millis(100);

const BRAILLE: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const LINE: [&str; 4] = ["-", "\\", "|", "/"];
const ARC: [&str; 6] = ["◜", "◠", "◝", "◞", "◡", "◟"];
const BOUNCE: [&str; 8] = ["⠁", "⠂", "⠄", "⡀", "⢀", "⠠", "⠐", "⠈"];
/// A bar that fills from the bottom and drains again. Braille, because the block elements
/// (U+2581 to U+2588) are East Asian Ambiguous and some terminals draw them two cells wide.
const BAR: [&str; 14] = [
    "⡀", "⣀", "⣄", "⣤", "⣦", "⣶", "⣷", "⣿", "⣷", "⣶", "⣦", "⣤", "⣄", "⣀",
];

/// A spinner's look: the frames it cycles through. Every frame is one cell wide, because the
/// transcript's and the tables' spinners are painted into a single cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpinnerStyle {
    Braille,
    Line,
    Arc,
    Bounce,
    Bar,
}

impl SpinnerStyle {
    /// Every style, in the order a random pick indexes them.
    pub const ALL: [SpinnerStyle; 5] = [
        SpinnerStyle::Braille,
        SpinnerStyle::Line,
        SpinnerStyle::Arc,
        SpinnerStyle::Bounce,
        SpinnerStyle::Bar,
    ];

    fn frames(self) -> &'static [&'static str] {
        match self {
            SpinnerStyle::Braille => &BRAILLE,
            SpinnerStyle::Line => &LINE,
            SpinnerStyle::Arc => &ARC,
            SpinnerStyle::Bounce => &BOUNCE,
            SpinnerStyle::Bar => &BAR,
        }
    }

    /// The frame for `elapsed` time since the app started.
    pub fn frame(self, elapsed: Duration) -> &'static str {
        let frames = self.frames();
        let step = elapsed.as_millis() / SPINNER_INTERVAL.as_millis();
        frames[(step % frames.len() as u128) as usize]
    }

    /// The style `setting` names, or for `Random` the one `seed` picks.
    pub fn pick(setting: SpinnerSetting, seed: u64) -> SpinnerStyle {
        match setting {
            SpinnerSetting::Random => Self::ALL[(seed % Self::ALL.len() as u64) as usize],
            SpinnerSetting::Braille => SpinnerStyle::Braille,
            SpinnerSetting::Line => SpinnerStyle::Line,
            SpinnerSetting::Arc => SpinnerStyle::Arc,
            SpinnerSetting::Bounce => SpinnerStyle::Bounce,
            SpinnerSetting::Bar => SpinnerStyle::Bar,
        }
    }
}

/// The braille spinner's frame for `elapsed`, which table cells hold until the table paints
/// the turn's style over them.
pub fn spinner_frame(elapsed: Duration) -> &'static str {
    SpinnerStyle::Braille.frame(elapsed)
}

/// The next state of the random spinner picks: one xorshift64 step. Zero is its fixed point,
/// so a Tui seeded with 0 picks braille on every turn.
pub fn next_seed(state: u64) -> u64 {
    let mut x = state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    x
}

/// The text after the spinner, naming what the agent is doing.
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

/// Whether the activity row shows `activity`. It gives way while one of the transcript's
/// animated markers already says the same and is among the `visible` lines: a reasoning
/// marker, the rows in `thinking`, while the agent thinks, or any other marker in `spinners`,
/// a tool call's, while it runs a tool.
pub fn shows_activity(
    activity: &Activity,
    spinners: &[usize],
    thinking: &[usize],
    visible: Range<usize>,
) -> bool {
    let shown = |line: &usize| visible.contains(line);
    match activity {
        Activity::Thinking => !thinking.iter().any(shown),
        Activity::Tool(_) => !spinners
            .iter()
            .filter(|line| !thinking.contains(line))
            .any(shown),
        _ => true,
    }
}

/// The whole activity row: `style`'s frame for `elapsed`, then the label.
pub fn activity_line(
    activity: &Activity,
    style: SpinnerStyle,
    elapsed: Duration,
    theme: &Theme,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{} ", style.frame(elapsed)), theme.accent),
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

    #[test]
    fn every_style_has_one_cell_frames_that_advance_and_wrap() {
        use unicode_width::UnicodeWidthStr;
        for style in SpinnerStyle::ALL {
            let frames = style.frames();
            assert!(frames.len() > 1, "{style:?}");
            for frame in frames {
                assert_eq!(frame.width(), 1, "{style:?} {frame:?}");
                // A terminal that draws East Asian Ambiguous glyphs two cells wide would spill
                // these into the next cell.
                assert_eq!(
                    frame.width_cjk(),
                    1,
                    "{style:?} {frame:?} is ambiguous width"
                );
            }
            assert_ne!(style.frame(Duration::ZERO), style.frame(SPINNER_INTERVAL));
            assert_eq!(
                style.frame(Duration::ZERO),
                style.frame(SPINNER_INTERVAL * frames.len() as u32),
                "{style:?}"
            );
        }
    }

    #[test]
    fn no_two_styles_share_their_frames() {
        for (i, a) in SpinnerStyle::ALL.iter().enumerate() {
            for b in &SpinnerStyle::ALL[i + 1..] {
                assert_ne!(a.frames(), b.frames(), "{a:?} and {b:?}");
            }
        }
    }

    #[test]
    fn a_setting_names_its_style_and_random_follows_the_seed() {
        assert_eq!(
            SpinnerStyle::pick(SpinnerSetting::Arc, 3),
            SpinnerStyle::Arc
        );
        assert_eq!(
            SpinnerStyle::pick(SpinnerSetting::Braille, 3),
            SpinnerStyle::Braille
        );
        for (seed, style) in SpinnerStyle::ALL.iter().enumerate() {
            let seed = seed as u64;
            assert_eq!(SpinnerStyle::pick(SpinnerSetting::Random, seed), *style);
            assert_eq!(
                SpinnerStyle::pick(
                    SpinnerSetting::Random,
                    seed + SpinnerStyle::ALL.len() as u64
                ),
                *style
            );
        }
    }

    #[test]
    fn a_zero_seed_stays_zero_and_others_keep_moving() {
        assert_eq!(next_seed(0), 0, "tests that pass 0 see the braille frames");
        let mut seed = 1;
        for _ in 0..100 {
            let next = next_seed(seed);
            assert_ne!(next, 0);
            assert_ne!(next, seed);
            seed = next;
        }
    }

    #[test]
    fn the_row_gives_way_only_to_an_animated_marker_on_screen() {
        let tools = Activity::Tool("linear__save_issue and 3 more".into());
        assert!(!shows_activity(&Activity::Thinking, &[5], &[5], 0..10));
        assert!(!shows_activity(&tools, &[3, 4, 5, 6], &[], 0..10));
        assert!(
            shows_activity(&Activity::Thinking, &[5], &[5], 6..16),
            "the marker is above the rows on screen"
        );
        assert!(
            shows_activity(&Activity::Thinking, &[16], &[16], 6..16),
            "the marker is below them"
        );
        assert!(
            shows_activity(&Activity::Thinking, &[], &[], 0..10),
            "expanded reasoning does not animate"
        );
        for other in [
            Activity::Waiting,
            Activity::Writing,
            Activity::Working,
            Activity::Interrupting,
        ] {
            assert!(shows_activity(&other, &[5], &[], 0..10), "{other:?}");
        }
    }

    #[test]
    fn thinking_gives_way_only_to_a_reasoning_marker_and_a_tool_only_to_a_tool_marker() {
        let tool = Activity::Tool("execute".into());
        assert!(
            shows_activity(&Activity::Thinking, &[5], &[], 0..10),
            "a tool's marker does not say the agent is thinking"
        );
        assert!(
            shows_activity(&tool, &[5], &[5], 0..10),
            "the reasoning marker does not say a tool runs"
        );
        assert!(!shows_activity(&tool, &[3, 5], &[5], 0..10));
    }
}
