use std::fmt::Write as _;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::channel::ReviewChannel;
use crate::error::AppError;
use crate::git_types::BlobOid;
use crate::path::RepoPath;
use crate::review::{ClassifiedFile, ReviewMetadata, ReviewState, Vetter};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HumanStatusOptions {
    pub show_all: bool,
    pub color: bool,
}

#[derive(Serialize)]
struct JsonStatus<'a> {
    channel: &'a ReviewChannel,
    files: Vec<JsonStatusRecord<'a>>,
}

#[derive(Serialize)]
struct JsonStatusRecord<'a> {
    path: &'a RepoPath,
    state: &'static str,
    blob: &'a BlobOid,
    baseline: Option<&'a BlobOid>,
    last_vetted_at: Option<&'a str>,
    vetted_by: Option<&'a Vetter>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StatusCounts {
    total: usize,
    vetted: usize,
    stale: usize,
    new: usize,
}

impl StatusCounts {
    fn from_classified(classified: &[ClassifiedFile]) -> Self {
        classified.iter().fold(
            Self {
                total: classified.len(),
                vetted: 0,
                stale: 0,
                new: 0,
            },
            |counts, file| match file.state {
                ReviewState::Vetted => Self {
                    vetted: counts.vetted + 1,
                    ..counts
                },
                ReviewState::Stale { .. } => Self {
                    stale: counts.stale + 1,
                    ..counts
                },
                ReviewState::New => Self {
                    new: counts.new + 1,
                    ..counts
                },
            },
        )
    }

    const fn needs_review(self) -> usize {
        self.stale + self.new
    }

    const fn all_vetted(self) -> bool {
        self.needs_review() == 0
    }

    const fn percent(self) -> usize {
        match self.total {
            0 => 100,
            total => (self.vetted * 100 + (total / 2)) / total,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Color {
    Green,
    Yellow,
    Red,
    Dim,
    Bold,
}

impl Color {
    const fn ansi_code(self) -> &'static str {
        match self {
            Self::Green => "32",
            Self::Yellow => "33",
            Self::Red => "31",
            Self::Dim => "2",
            Self::Bold => "1",
        }
    }
}

pub(crate) fn json_status(
    channel: &ReviewChannel,
    classified: &[ClassifiedFile],
) -> Result<String, AppError> {
    let files = classified
        .iter()
        .map(|file| JsonStatusRecord {
            path: &file.path,
            state: file.state.as_str(),
            blob: &file.blob,
            baseline: file.state.baseline(),
            last_vetted_at: file
                .metadata
                .as_ref()
                .map(|metadata| metadata.last_vetted_at.as_str()),
            vetted_by: file.metadata.as_ref().map(|metadata| &metadata.vetted_by),
        })
        .collect::<Vec<_>>();
    serde_json::to_string_pretty(&JsonStatus { channel, files })
        .map(|json| format!("{json}\n"))
        .map_err(AppError::from)
}

pub(crate) fn human_status(
    channel: &ReviewChannel,
    classified: &[ClassifiedFile],
    options: HumanStatusOptions,
) -> String {
    let counts = StatusCounts::from_classified(classified);
    let mut output = String::new();
    push_header(&mut output, channel, counts, options.color);

    if counts.all_vetted() {
        let _ = writeln!(
            output,
            "  {} All files are vetted.",
            paint("✓", Color::Green, options.color)
        );
        return output;
    }

    let _ = writeln!(
        output,
        "  {} {} {} review: {} new, {} stale\n",
        counts.needs_review(),
        plural(counts.needs_review(), "file", "files"),
        need_verb(counts.needs_review()),
        counts.new,
        counts.stale
    );

    push_actionable_group(
        &mut output,
        "New — never reviewed",
        classified,
        |state| matches!(state, ReviewState::New),
        "✗",
        Color::Red,
        options.color,
    );
    push_actionable_group(
        &mut output,
        "Stale — changed since last review",
        classified,
        |state| matches!(state, ReviewState::Stale { .. }),
        "~",
        Color::Yellow,
        options.color,
    );

    if options.show_all {
        push_vetted_group(&mut output, classified, options.color);
    } else {
        push_hidden_vetted_summary(&mut output, counts, options.color);
    }

    push_next_steps(&mut output, options.color);
    output
}

pub(crate) fn check_status(channel: &ReviewChannel, classified: &[ClassifiedFile]) -> String {
    let counts = StatusCounts::from_classified(classified);
    let mut output = String::new();

    if counts.all_vetted() {
        let _ = writeln!(output, "Review gate passed for channel {channel}.");
        let _ = writeln!(
            output,
            "All {} {} are vetted.",
            counts.total,
            plural(counts.total, "file", "files")
        );
        return output;
    }

    let _ = writeln!(output, "Review gate failed for channel {channel}.\n");
    let _ = writeln!(
        output,
        "{} {} {} review:",
        counts.needs_review(),
        plural(counts.needs_review(), "file", "files"),
        need_verb(counts.needs_review())
    );
    classified
        .iter()
        .filter(|file| !matches!(file.state, ReviewState::Vetted))
        .for_each(|file| {
            let _ = writeln!(output, "  {:<6} {}", file.state.as_str(), file.path);
        });
    output
}

fn push_header(
    output: &mut String,
    channel: &ReviewChannel,
    counts: StatusCounts,
    color_enabled: bool,
) {
    let _ = writeln!(
        output,
        "{}\n",
        paint(
            &format!("git vet · channel {channel}"),
            Color::Bold,
            color_enabled
        )
    );
    let bar = progress_bar(counts.vetted, counts.total, 20);
    let _ = writeln!(
        output,
        "  {}  {}/{} vetted · {}%\n",
        paint(&bar, progress_color(counts), color_enabled),
        counts.vetted,
        counts.total,
        counts.percent()
    );
}

const fn progress_color(counts: StatusCounts) -> Color {
    if counts.all_vetted() {
        Color::Green
    } else {
        Color::Yellow
    }
}

fn progress_bar(vetted: usize, total: usize, width: usize) -> String {
    let filled = match total {
        0 => width,
        total => (vetted * width + (total / 2)) / total,
    };
    let empty = width.saturating_sub(filled);
    format!("{}{}", "█".repeat(filled), "░".repeat(empty))
}

fn push_actionable_group(
    output: &mut String,
    title: &str,
    classified: &[ClassifiedFile],
    include: impl Fn(&ReviewState) -> bool,
    symbol: &str,
    color: Color,
    color_enabled: bool,
) {
    let files = classified
        .iter()
        .filter(|file| include(&file.state))
        .collect::<Vec<_>>();
    if files.is_empty() {
        return;
    }

    let _ = writeln!(output, "{}:", paint(title, Color::Bold, color_enabled));
    push_file_lines(
        output,
        &files,
        symbol,
        color,
        color_enabled,
        MetadataVerb::LastReviewed,
    );
    output.push('\n');
}

fn push_hidden_vetted_summary(output: &mut String, counts: StatusCounts, color_enabled: bool) {
    if counts.vetted == 0 {
        return;
    }

    let _ = writeln!(
        output,
        "{}\n",
        paint(
            &format!(
                "{} vetted {} hidden. Use `git vet status --all` to show them.",
                counts.vetted,
                plural(counts.vetted, "file", "files")
            ),
            Color::Dim,
            color_enabled
        )
    );
}

fn push_vetted_group(output: &mut String, classified: &[ClassifiedFile], color_enabled: bool) {
    let files = classified
        .iter()
        .filter(|file| matches!(file.state, ReviewState::Vetted))
        .collect::<Vec<_>>();
    if files.is_empty() {
        return;
    }

    let _ = writeln!(output, "{}:", paint("Vetted", Color::Bold, color_enabled));
    push_file_lines(
        output,
        &files,
        "✓",
        Color::Green,
        color_enabled,
        MetadataVerb::Reviewed,
    );
    output.push('\n');
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MetadataVerb {
    LastReviewed,
    Reviewed,
}

impl MetadataVerb {
    const fn as_str(self) -> &'static str {
        match self {
            Self::LastReviewed => "last reviewed",
            Self::Reviewed => "reviewed",
        }
    }
}

fn push_file_lines(
    output: &mut String,
    files: &[&ClassifiedFile],
    symbol: &str,
    color: Color,
    color_enabled: bool,
    metadata_verb: MetadataVerb,
) {
    let path_width = files
        .iter()
        .map(|file| file.path.to_string().chars().count())
        .max()
        .unwrap_or(0);

    for file in files {
        let metadata = file
            .metadata
            .as_ref()
            .map(|metadata| format_metadata(metadata, metadata_verb))
            .unwrap_or_default();
        let rendered_metadata = if metadata.is_empty() {
            String::new()
        } else {
            paint(&metadata, Color::Dim, color_enabled)
        };
        let _ = writeln!(
            output,
            "  {} {:<path_width$}{}",
            paint(symbol, color, color_enabled),
            file.path,
            rendered_metadata,
        );
    }
}

fn format_metadata(metadata: &ReviewMetadata, verb: MetadataVerb) -> String {
    let reviewed_at =
        relative_time(&metadata.last_vetted_at).unwrap_or_else(|| metadata.last_vetted_at.clone());
    format!(
        "  {} {reviewed_at} by {}",
        verb.as_str(),
        metadata.vetted_by.name
    )
}

fn relative_time(timestamp: &str) -> Option<String> {
    let reviewed_at = DateTime::parse_from_rfc3339(timestamp).ok()?;
    let elapsed = Utc::now().signed_duration_since(reviewed_at.with_timezone(&Utc));

    if elapsed.num_seconds() < 0 {
        return Some("just now".to_owned());
    }

    match elapsed.num_days() {
        days if days >= 365 => Some(relative_unit(days / 365, "year", "years")),
        days if days >= 30 => Some(relative_unit(days / 30, "month", "months")),
        days if days >= 1 => Some(relative_unit(days, "day", "days")),
        _ => match elapsed.num_hours() {
            hours if hours >= 1 => Some(relative_unit(hours, "hour", "hours")),
            _ => match elapsed.num_minutes() {
                minutes if minutes >= 1 => Some(relative_unit(minutes, "minute", "minutes")),
                _ => Some("just now".to_owned()),
            },
        },
    }
}

fn relative_unit(value: i64, singular: &str, plural_form: &str) -> String {
    let unit = match value {
        1 => singular,
        _ => plural_form,
    };
    format!("{value} {unit} ago")
}

fn push_next_steps(output: &mut String, color_enabled: bool) {
    let _ = writeln!(output, "{}", paint("Next:", Color::Bold, color_enabled));
    output.push_str("  git vet diff <path>\n");
    output.push_str("  git vet mark <path>\n");
}

const fn plural<'a>(count: usize, singular: &'a str, plural_form: &'a str) -> &'a str {
    match count {
        1 => singular,
        _ => plural_form,
    }
}

const fn need_verb(count: usize) -> &'static str {
    match count {
        1 => "needs",
        _ => "need",
    }
}

fn paint(text: &str, color: Color, enabled: bool) -> String {
    if enabled {
        format!("\u{1b}[{}m{text}\u{1b}[0m", color.ansi_code())
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, SecondsFormat};

    use super::*;
    use crate::review::ReviewMetadata;

    #[test]
    fn progress_bar_renders_zero_total_as_complete() {
        assert_eq!(progress_bar(0, 0, 5), "█████");
    }

    #[test]
    fn relative_time_renders_recent_days() {
        let timestamp = (Utc::now() - Duration::days(3)).to_rfc3339_opts(SecondsFormat::Secs, true);

        assert_eq!(relative_time(&timestamp), Some("3 days ago".to_owned()));
    }

    #[test]
    fn metadata_uses_vetter_name_without_email() {
        let metadata = ReviewMetadata {
            last_vetted_at: "not-a-date".to_owned(),
            vetted_by: Vetter::new("Alice".to_owned(), "alice@example.com".to_owned()),
        };

        assert_eq!(
            format_metadata(&metadata, MetadataVerb::Reviewed),
            "  reviewed not-a-date by Alice"
        );
    }
}
