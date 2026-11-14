//! The view of a progress report for a person at a terminal: a title, a bar
//! and one line per item. It shortens what it shows: a topic for a filename,
//! an age for a time. `--plain` and `--json` carry every recorded fact.

use super::{
    Counts, ItemRow, ProgressReport, ProjectProgress, RelatedReport, SeriesReport, WaitingQuestion,
};
use crate::terminal::{fill, Look, BOLD, CYAN, DIM, GREEN, RED, YELLOW};
use chrono::{DateTime, Utc};

/// The bar's cells.
const BAR: usize = 20;
/// The widest the title column grows. A longer title continues on the next
/// line, under itself.
const TITLE_COLUMN: usize = 56;
/// The columns a narrow terminal keeps free after a stage, enough for a
/// revision and an age.
const DETAIL: usize = 20;

pub(super) struct View {
    look: Look,
    now: DateTime<Utc>,
    lines: Vec<String>,
}

impl View {
    pub(super) fn new(look: Look, now: DateTime<Utc>) -> Self {
        Self {
            look,
            now,
            lines: Vec::new(),
        }
    }

    pub(super) fn finish(self) -> String {
        self.lines.join("\n")
    }

    /// One project's report: its series, the related series of other
    /// projects, then the questions that wait on a person.
    pub(super) fn project(&mut self, report: &ProgressReport) {
        if report.series.is_empty() {
            self.lines.push(
                "no active progress series; start one with `arc progress start <topic> --title <title>`"
                    .to_string(),
            );
        }
        for series in &report.series {
            self.blank();
            self.series(series);
        }
        if !report.related.is_empty() {
            self.section("related series in other projects");
            for related in &report.related {
                self.blank();
                self.related(related);
            }
        }
        if !report.waiting_on_person.is_empty() {
            self.section(&format!(
                "waiting on a person ({})",
                report.waiting_on_person.len()
            ));
            self.blank();
            for question in &report.waiting_on_person {
                self.question(question);
            }
        }
    }

    /// Every project's series, each project under its own heading.
    pub(super) fn workspace(&mut self, projects: &[ProjectProgress]) {
        if projects.is_empty() {
            self.lines.push("no active progress series".to_string());
        }
        for project in projects {
            self.blank();
            let count = match project.series.len() {
                1 => "(1 series)".to_string(),
                count => format!("({count} series)"),
            };
            self.lines.push(format!(
                "{} {} {}",
                self.dim("──"),
                self.look.paint(&format!("{BOLD};{CYAN}"), &project.project),
                self.dim(&count)
            ));
            for series in &project.series {
                self.blank();
                self.series(series);
            }
        }
    }

    fn dim(&self, text: &str) -> String {
        self.look.paint(DIM, text)
    }

    /// A blank line, unless the view is empty or already ends with one.
    fn blank(&mut self) {
        if self.lines.last().is_some_and(|line| !line.is_empty()) {
            self.lines.push(String::new());
        }
    }

    /// The faint line of small facts under a bar.
    fn facts(&mut self, facts: &[String]) {
        for text in self.look.wrap(&facts.join(" · "), 0) {
            self.lines.push(self.dim(&text));
        }
    }

    fn section(&mut self, name: &str) {
        self.blank();
        self.lines.push(format!(
            "{} {}",
            self.dim("──"),
            self.look.paint(BOLD, name)
        ));
    }

    fn series(&mut self, series: &SeriesReport) {
        self.lines.push(self.look.paint(BOLD, &series.title));
        self.lines.push(self.bar(&series.counts, None));
        if !series.related.is_empty() {
            let named: Vec<String> = series
                .related
                .iter()
                .map(|relation| {
                    let (arrow, state) = match relation.mutual {
                        Some(true) => ("↔", ""),
                        Some(false) => ("→", " (one-sided)"),
                        None => ("→", " (unread)"),
                    };
                    format!(
                        "{arrow} {}::{}{}",
                        self.look.paint(CYAN, &relation.project),
                        topic(&relation.file),
                        self.dim(state)
                    )
                })
                .collect();
            self.lines.push(named.join("  "));
        }
        let mut facts = vec![topic(&series.file)];
        if let Some(retired) = &series.retired {
            facts.push(match &retired.by {
                Some(by) => format!("retired {} by {by}", self.age(&retired.at)),
                None => format!("retired {}", self.age(&retired.at)),
            });
        }
        if let Some(plan) = &series.follows {
            facts.push(format!("follows {plan}"));
        }
        let start = series.counts.since_start;
        let edits: Vec<String> = [
            (start.added, "added"),
            (start.dropped, "dropped"),
            (start.split, "split"),
        ]
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, edit)| format!("{count} {edit}"))
        .collect();
        if !edits.is_empty() {
            facts.push(format!("{} since the start", edits.join(", ")));
        }
        self.facts(&facts);
        if !series.items.is_empty() {
            self.blank();
            self.rows(&series.items);
        }
    }

    /// Another project's series in compact form: its bar and what is in
    /// flight there.
    fn related(&mut self, related: &RelatedReport) {
        let project = self.look.paint(CYAN, &related.project);
        let Some(counts) = &related.counts else {
            self.lines.push(format!(
                "{project}::{}  {} {}",
                topic(&related.file),
                self.look.paint(RED, "unavailable"),
                self.dim(&format!(
                    "({})",
                    related.unavailable.as_deref().unwrap_or("unread")
                ))
            ));
            return;
        };
        self.lines.push(format!(
            "{project} {} {}",
            self.dim("·"),
            self.look
                .paint(BOLD, related.title.as_deref().unwrap_or_default())
        ));
        let landed = related
            .last_integration
            .as_ref()
            .map(|landing| format!("last integrated {}", self.age(&landing.at)));
        self.lines.push(self.bar(counts, landed));
        let mut facts = vec![topic(&related.file)];
        if let Some(retired) = &related.retired {
            facts.push(format!("retired {}", self.age(&retired.at)));
        }
        let ours: Vec<String> = related
            .related_from
            .iter()
            .map(|file| topic(file))
            .collect();
        if !ours.is_empty() {
            facts.push(format!("related to {}", ours.join(", ")));
        }
        self.facts(&facts);
        self.rows(&related.in_flight);
    }

    /// The bar and the counts it draws, each count in its cells' colour.
    fn bar(&self, counts: &Counts, last: Option<String>) -> String {
        let rest = counts
            .items
            .saturating_sub(counts.complete + counts.in_flight);
        let [complete, in_flight, open] = cells([counts.complete, counts.in_flight, rest], BAR);
        let bar = format!(
            "{}{}{}",
            self.look.paint(GREEN, &"█".repeat(complete)),
            self.look.paint(YELLOW, &"▒".repeat(in_flight)),
            self.dim(&"░".repeat(open))
        );
        let share = match counts.percent {
            Some(percent) => format!(
                "{} {} {percent}%",
                self.look
                    .paint(GREEN, &format!("{} of {}", counts.complete, counts.items)),
                self.dim("·")
            ),
            None => self.dim("no items"),
        };
        let mut rest = Vec::new();
        if counts.in_flight > 0 {
            rest.push(
                self.look
                    .paint(YELLOW, &format!("{} in flight", counts.in_flight)),
            );
        }
        if counts.open > 0 {
            rest.push(format!("{} open", counts.open));
        }
        if counts.closed > 0 {
            rest.push(self.look.paint(
                RED,
                &format!("{} closed without integrating", counts.closed),
            ));
        }
        rest.extend(last.map(|last| self.dim(&last)));
        if rest.is_empty() {
            format!("{bar}  {share}")
        } else {
            format!("{bar}  {share}   {}", rest.join(&self.dim(" · ")))
        }
    }

    /// One line per item: a mark, its id and title, then the stage and what
    /// the ledger records beside it. An open item carries its mark alone. A
    /// title wider than its column continues under itself, and a note takes
    /// lines of its own there.
    fn rows(&mut self, rows: &[ItemRow]) {
        let ids = rows.iter().map(|row| row.id.len()).max().unwrap_or(0);
        let indent = ids + 6;
        let staged = || rows.iter().filter(|row| row.stage != "open");
        let stages = staged().map(|row| row.stage.len()).max().unwrap_or(0);
        let column = self.look.width.map_or(TITLE_COLUMN, |width| {
            width
                .saturating_sub(indent + stages + DETAIL + 4)
                .clamp(24, TITLE_COLUMN)
        });
        let titles = staged()
            .map(|row| row.title.chars().count())
            .max()
            .unwrap_or(0)
            .min(column);
        for row in rows {
            let (mark, style) = mark(row.stage);
            let title = if row.stage != "open" && row.title.chars().count() > titles {
                fill(&row.title, titles)
            } else {
                vec![row.title.clone()]
            };
            let faded = |text: &str| match row.stage {
                "dropped" | "split" => self.dim(text),
                _ => text.to_string(),
            };
            let mut line = format!(
                "  {} {}{}  {}",
                self.look.paint(style, mark),
                self.dim(&row.id),
                " ".repeat(ids - row.id.len()),
                faded(&title[0])
            );
            if row.stage != "open" {
                line.push_str(&" ".repeat(titles.saturating_sub(title[0].chars().count())));
                line.push_str("  ");
                line.push_str(&self.look.paint(style, row.stage));
                let detail = self.detail(row);
                if !detail.is_empty() {
                    line.push_str(&" ".repeat(stages - row.stage.len()));
                    line.push_str("  ");
                    line.push_str(&detail);
                }
            }
            let mut lines = vec![line];
            for text in &title[1..] {
                lines.push(format!("{}{}", " ".repeat(indent), faded(text)));
            }
            for text in row
                .note
                .iter()
                .flat_map(|note| self.look.wrap(note, indent))
            {
                lines.push(format!("{}{}", " ".repeat(indent), self.dim(&text)));
            }
            self.lines.extend(lines);
        }
    }

    fn detail(&self, row: &ItemRow) -> String {
        let mut parts = Vec::new();
        if row.stage == "integrated" {
            if let Some(revision) = &row.revision {
                parts.push(self.dim(&revision.chars().take(8).collect::<String>()));
            }
            if let Some(at) = &row.integrated_at {
                parts.push(self.dim(&self.age(at)));
            }
        } else {
            parts.extend(row.verdict.map(str::to_string));
            if let Some(change) = &row.change {
                parts.push(self.dim(change));
            }
            match row.changes_requested_rounds {
                0 => {}
                1 => parts.push(self.dim("1 round asked for changes")),
                rounds => parts.push(self.dim(&format!("{rounds} rounds asked for changes"))),
            }
            if let Some(holder) = &row.held_by {
                parts.push(self.dim(&format!("held by {holder}")));
            }
        }
        if !row.split_into.is_empty() {
            parts.push(self.dim(&format!("into {}", row.split_into.join(", "))));
        }
        parts.join(&self.dim(" · "))
    }

    fn question(&mut self, question: &WaitingQuestion) {
        let handle = format!(
            "{} {} · asked {}",
            question.file,
            question.question,
            self.age(&question.asked_at)
        );
        let mark = self.look.paint(YELLOW, "?");
        let Some(heading) = &question.heading else {
            self.lines.push(format!("  {mark} {}", self.dim(&handle)));
            return;
        };
        for (index, text) in self.look.wrap(heading, 4).into_iter().enumerate() {
            self.lines.push(match index {
                0 => format!("  {mark} {text}"),
                _ => format!("    {text}"),
            });
        }
        self.lines.push(format!("    {}", self.dim(&handle)));
    }

    /// How long ago a recorded time was, to one unit; the time as recorded
    /// when it does not parse or is yet to come.
    fn age(&self, at: &str) -> String {
        let Ok(then) = DateTime::parse_from_rfc3339(at) else {
            return at.to_string();
        };
        if then > self.now {
            return at.to_string();
        }
        match (self.now - then.with_timezone(&Utc)).num_seconds() {
            ..=59 => "just now".to_string(),
            seconds @ 60..=3_599 => format!("{}m ago", seconds / 60),
            seconds @ 3_600..=172_799 => format!("{}h ago", seconds / 3_600),
            seconds => format!("{}d ago", seconds / 86_400),
        }
    }
}

/// The topic a series is named by on the command line, or its filename when
/// that is not an artifact's name.
fn topic(file: &str) -> String {
    crate::journal::parse_artifact_name(file)
        .map(|(_, topic, _)| topic)
        .unwrap_or_else(|| file.to_string())
}

/// The mark that stands for a stage, and the colour the mark and the stage
/// take. A linked item's mark fills as its change moves on.
fn mark(stage: &str) -> (&'static str, &'static str) {
    match stage {
        "integrated" | "done" => ("✓", GREEN),
        "opened" => ("◔", YELLOW),
        "snapshotted" => ("◑", YELLOW),
        "verified" => ("◕", YELLOW),
        "reviewed" => ("●", YELLOW),
        "open" => ("○", DIM),
        "dropped" => ("×", DIM),
        "split" => ("÷", DIM),
        "closed" => ("×", RED),
        _ => ("?", RED),
    }
}

/// How many of `cells` each part takes, in proportion. A part with any share
/// keeps at least one cell, so the bar is full only when one part is all
/// there is. With nothing to share, every cell is the last part's.
fn cells(parts: [usize; 3], cells: usize) -> [usize; 3] {
    let total: usize = parts.iter().sum();
    if total == 0 {
        return [0, 0, cells];
    }
    let mut taken = parts.map(|part| part * cells / total);
    let mut by_remainder = [0, 1, 2];
    by_remainder.sort_by_key(|&part| std::cmp::Reverse(parts[part] * cells % total));
    let spare = cells - taken.iter().sum::<usize>();
    for &part in by_remainder.iter().take(spare) {
        taken[part] += 1;
    }
    for part in 0..3 {
        if parts[part] > 0 && taken[part] == 0 {
            let widest = (0..3).max_by_key(|&other| taken[other]).unwrap_or(part);
            taken[widest] -= 1;
            taken[part] = 1;
        }
    }
    taken
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::series::SinceStart;

    fn look(color: bool, width: Option<usize>) -> Look {
        Look { color, width }
    }

    fn now() -> DateTime<Utc> {
        "2026-01-02T12:00:00Z".parse().unwrap()
    }

    fn row(id: &str, title: &str, stage: &'static str) -> ItemRow {
        ItemRow {
            id: id.to_string(),
            title: title.to_string(),
            stage,
            change: None,
            verdict: None,
            held_by: None,
            changes_requested_rounds: 0,
            integrated_at: None,
            revision: None,
            note: None,
            split_into: Vec::new(),
        }
    }

    fn series(items: Vec<ItemRow>) -> SeriesReport {
        let standing = |row: &&ItemRow| !matches!(row.stage, "dropped" | "split");
        let count = |stages: &[&str]| {
            items
                .iter()
                .filter(|row| stages.contains(&row.stage))
                .count()
        };
        let complete = count(&["integrated", "done"]);
        let total = items.iter().filter(standing).count();
        SeriesReport {
            file: "20260102T000000Z-ship-it-progress.md".to_string(),
            title: "Ship it".to_string(),
            follows: None,
            started_at: None,
            retired: None,
            related: Vec::new(),
            counts: Counts {
                items: total,
                complete,
                integrated: count(&["integrated"]),
                done: count(&["done"]),
                in_flight: count(&["opened", "snapshotted", "verified", "reviewed"]),
                open: count(&["open"]),
                closed: count(&["closed"]),
                percent: (total > 0).then(|| complete * 100 / total),
                since_start: SinceStart {
                    declared: total,
                    added: 1,
                    ..SinceStart::default()
                },
            },
            last_integration: None,
            items,
        }
    }

    fn sample() -> SeriesReport {
        let mut landed = row("i1", "Lay the keel", "integrated");
        landed.change = Some("keel-abc123".to_string());
        landed.integrated_at = Some("2026-01-02T09:00:00Z".to_string());
        landed.revision = Some("0123456789abcdef".to_string());
        let mut moving = row("i2", "Raise the mast", "reviewed");
        moving.change = Some("mast-def456".to_string());
        moving.verdict = Some("approved");
        let mut noted = row("i3", "Paint", "done");
        noted.note = Some("two coats were enough for the whole of the hull".to_string());
        series(vec![landed, moving, noted, row("i10", "Launch", "open")])
    }

    fn drawn(look: Look, series: &SeriesReport) -> String {
        let mut view = View::new(look, now());
        view.series(series);
        view.finish()
    }

    /// The view of a series, unpainted, is these lines: the bar's cells in
    /// proportion, each stage after an aligned title, the open item bare,
    /// the note under its title. Rejects a view that drops an item, shows a
    /// stage the report does not hold, or pads a line with trailing space.
    #[test]
    fn a_series_draws_its_bar_and_one_line_per_item() {
        let text = drawn(look(false, None), &sample());
        assert_eq!(
            text,
            "\
Ship it
██████████▒▒▒▒▒░░░░░  2 of 4 · 50%   1 in flight · 1 open
ship-it · 1 added since the start

  ✓ i1   Lay the keel    integrated  01234567 · 3h ago
  ● i2   Raise the mast  reviewed    approved · mast-def456
  ✓ i3   Paint           done
         two coats were enough for the whole of the hull
  ○ i10  Launch"
        );
        assert!(text.lines().all(|line| line == line.trim_end()));
    }

    /// Colour paints the same text and adds nothing else. Rejects a painted
    /// view whose words or alignment differ from the unpainted one.
    #[test]
    fn colour_only_paints_the_text() {
        let painted = drawn(look(true, None), &sample());
        assert!(painted.contains("\x1b[32m✓\x1b[0m"), "{painted:?}");
        let mut stripped = String::new();
        let mut rest = painted.as_str();
        while let Some(start) = rest.find("\x1b[") {
            stripped.push_str(&rest[..start]);
            rest = &rest[start..];
            rest = &rest[rest.find('m').unwrap() + 1..];
        }
        stripped.push_str(rest);
        assert_eq!(stripped, drawn(look(false, None), &sample()));
    }

    /// A note wraps under its title at the terminal's width. Rejects a note
    /// that runs past the edge and returns to the first column.
    #[test]
    fn a_note_wraps_beside_its_indent() {
        let text = drawn(look(false, Some(40)), &sample());
        let note: Vec<&str> = text
            .lines()
            .filter(|line| line.starts_with("         "))
            .collect();
        assert_eq!(
            note,
            [
                "         two coats were enough for the",
                "         whole of the hull"
            ]
        );
    }

    /// Every part with a share shows in the bar and the cells sum to its
    /// width. Rejects a bar that reads complete while an item is open, and
    /// one that hides the single item in flight among many.
    #[test]
    fn the_bar_keeps_a_cell_for_every_part_that_has_a_share() {
        assert_eq!(cells([4, 1, 3], 20), [10, 3, 7]);
        assert_eq!(cells([99, 0, 1], 20), [19, 0, 1]);
        assert_eq!(cells([0, 1, 99], 20), [0, 1, 19]);
        assert_eq!(cells([5, 0, 0], 20), [20, 0, 0]);
        assert_eq!(cells([0, 0, 0], 20), [0, 0, 20]);
        for parts in [[1, 1, 1], [7, 2, 4], [1, 30, 2]] {
            assert_eq!(cells(parts, 20).iter().sum::<usize>(), 20, "{parts:?}");
        }
    }

    /// A title wider than its column continues under itself with the stage
    /// on its first line, and a dropped item stays in the list and outside
    /// the bar. Rejects a cut title and a stage pushed out of its column.
    #[test]
    fn a_long_title_continues_under_itself() {
        let long = "word ".repeat(20);
        let mut dropped = row("i2", "Lose", "dropped");
        dropped.note = Some("out of scope".to_string());
        let text = drawn(
            look(false, None),
            &series(vec![row("i1", long.trim(), "done"), dropped]),
        );
        let first = "word ".repeat(11);
        let second = "word ".repeat(9);
        let rows: Vec<&str> = text.lines().skip(4).collect();
        assert_eq!(
            rows,
            [
                format!("  ✓ i1  {}    done", first.trim()),
                format!("        {}", second.trim()),
                format!("  × i2  Lose{}  dropped", " ".repeat(52)),
                "        out of scope".to_string(),
            ]
        );
        assert!(text.contains("1 of 1 · 100%"), "{text}");
    }

    /// A retired series names the artifact recorded as resolving it, and a
    /// time yet to come prints as recorded. Rejects a view that calls every
    /// retirement a supersession, and an age that reads a future time as
    /// just past.
    #[test]
    fn retirement_and_future_times_print_only_what_is_recorded() {
        let mut retired = series(vec![row("i1", "Keep", "done")]);
        retired.retired = Some(crate::journal::series::Retirement {
            at: "2026-01-01T12:00:00Z".to_string(),
            by: Some("20260101T000000Z-ship-it-decision.md".to_string()),
        });
        let text = drawn(look(false, None), &retired);
        assert!(
            text.contains("ship-it · retired 24h ago by 20260101T000000Z-ship-it-decision.md"),
            "{text}"
        );
        assert!(!text.contains("superseded"), "{text}");

        let mut ahead = row("i1", "Keep", "integrated");
        ahead.integrated_at = Some("2031-05-06T07:08:09Z".to_string());
        let text = drawn(look(false, None), &series(vec![ahead]));
        assert!(text.contains("integrated  2031-05-06T07:08:09Z"), "{text}");
        assert!(!text.contains("just now"), "{text}");

        let view = View::new(
            look(false, None),
            "2026-01-02T11:59:59.500Z".parse().unwrap(),
        );
        assert_eq!(view.age("2026-01-02T12:00:00Z"), "2026-01-02T12:00:00Z");
        assert_eq!(view.age("2026-01-02T11:59:59Z"), "just now");
    }

    /// A narrow terminal narrows the title column, so the stage and a short
    /// detail still fit its width. Rejects an item line wider than the
    /// terminal while its title has room to continue below.
    #[test]
    fn a_narrow_terminal_narrows_the_title_column() {
        let mut landed = row("i1", &"word ".repeat(12), "integrated");
        landed.title = landed.title.trim().to_string();
        landed.integrated_at = Some("2026-01-02T09:00:00Z".to_string());
        landed.revision = Some("0123456789abcdef".to_string());
        let text = drawn(look(false, Some(72)), &series(vec![landed]));
        let rows: Vec<&str> = text.lines().skip(4).collect();
        assert_eq!(rows.len(), 2, "{text}");
        assert!(rows[0].ends_with("integrated  01234567 · 3h ago"), "{text}");
        assert!(rows.iter().all(|row| row.chars().count() <= 72), "{text}");
    }
}
