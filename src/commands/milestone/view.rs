//! Terminal view for `arc milestone`: bold header, proportional critical bar,
//! separate sideband summary, and a painted wave-by-track matrix. Unpainted
//! output carries the exact same text without ANSI escapes; `--plain` and
//! `--json` carry every recorded fact.

use super::matrix::{MatrixCounts, MilestoneReport, TrackReport, WaveCell};
use crate::journal::milestone::TrackRole;
use crate::terminal::{Look, BOLD, CYAN, DIM, GREEN, RED, YELLOW};
use chrono::{DateTime, Utc};

const BAR: usize = 20;

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

    pub(super) fn milestones(&mut self, milestones: &[MilestoneReport], ready_only: bool) {
        if milestones.is_empty() {
            self.lines.push("No open milestones.".to_string());
            return;
        }
        for (idx, ms) in milestones.iter().enumerate() {
            if idx > 0 {
                self.blank();
            }
            self.milestone(ms, ready_only);
        }
    }

    pub(super) fn workspace(&mut self, projects: &[super::ProjectMilestones]) {
        if projects.is_empty() {
            self.lines.push("No active milestones.".to_string());
            return;
        }
        for project in projects {
            self.blank();
            let count = match project.milestones.len() {
                1 => "(1 milestone)".to_string(),
                n => format!("({n} milestones)"),
            };
            self.lines.push(format!(
                "{} {} {}",
                self.dim("──"),
                self.look.paint(&format!("{BOLD};{CYAN}"), &project.project),
                self.dim(&count)
            ));
            for ms in &project.milestones {
                self.blank();
                self.milestone(ms, false);
            }
        }
    }

    pub(super) fn milestone(&mut self, ms: &MilestoneReport, ready_only: bool) {
        self.lines.push(format!(
            "{}  {}",
            self.look.paint(BOLD, &ms.title),
            self.dim(&topic(&ms.file))
        ));

        let mut facts = Vec::new();
        if let Some(plan) = &ms.follows {
            facts.push(format!("follows {plan}"));
        }
        if ms.advisory_waves {
            facts.push("advisory waves".to_string());
        }
        if let Some(closure) = &ms.closure {
            let mut closed = format!("closed {} [{}]", self.age(&closure.at), closure.outcome);
            if let Some(cp) = &closure.checkpoint {
                closed.push_str(&format!(" at checkpoint {}", cp.name));
            }
            facts.push(closed);
        }
        if !facts.is_empty() {
            for line in self.look.wrap(&facts.join(" · "), 2) {
                self.lines.push(format!("  {}", self.dim(&line)));
            }
        }

        if ms.counts.critical.items > 0 || !ms.tracks.is_empty() {
            self.lines.push(format!(
                "  {}  Critical: {}",
                self.bar(&ms.counts.critical),
                self.counts_text(&ms.counts.critical)
            ));
        }
        if ms.counts.sideband.items > 0 {
            self.lines.push(format!(
                "  {}  Sideband: {}",
                self.look.paint(CYAN, "sideband"),
                self.counts_text(&ms.counts.sideband)
            ));
        }
        if ms.counts.unavailable_tracks > 0 {
            self.lines.push(format!(
                "  {}",
                self.look.paint(
                    RED,
                    &format!("Unavailable tracks: {}", ms.counts.unavailable_tracks)
                )
            ));
        }

        for wave in &ms.waves {
            let (policy, policy_style) = if wave.effective_gated {
                ("gated", YELLOW)
            } else {
                ("advisory", DIM)
            };
            let title = wave
                .title
                .as_deref()
                .map(|t| format!(" {t}"))
                .unwrap_or_default();
            let ratio = format!("{}/{}", wave.critical.complete, wave.critical.items);
            let ratio_styled =
                if wave.critical.items > 0 && wave.critical.complete == wave.critical.items {
                    self.look.paint(GREEN, &ratio)
                } else {
                    ratio
                };
            self.lines.push(format!(
                "  Wave {} [{}]{title}  critical {ratio_styled}",
                self.look.paint(BOLD, &wave.id),
                self.look.paint(policy_style, policy)
            ));
        }

        if ready_only {
            self.ready_list(ms);
        } else if !ms.tracks.is_empty() {
            self.blank();
            self.matrix_table(ms);
        }

        for cp in &ms.checkpoints {
            self.lines.push(format!(
                "  {} {} {}",
                self.dim("Checkpoint"),
                self.look.paint(CYAN, &cp.name),
                self.dim(&format!(
                    "at {} (event {})",
                    crate::render::short_sha(&cp.revision),
                    cp.event_id
                ))
            ));
        }
        if let Some(closure) = &ms.closure {
            if let Some(by) = &closure.by {
                self.lines
                    .push(format!("  {}", self.dim(&format!("By {by}"))));
            }
            if let Some(note) = &closure.note {
                for wrapped in self.look.wrap(note, 4) {
                    self.lines.push(format!("    {}", self.dim(&wrapped)));
                }
            }
        }
    }

    fn ready_list(&mut self, ms: &MilestoneReport) {
        if ms.ready.is_empty() {
            self.lines
                .push(format!("  {}", self.dim("No ready items.")));
            return;
        }
        self.lines.push(format!(
            "  {} ({}):",
            self.look.paint(GREEN, "Ready"),
            ms.ready.len()
        ));
        for item in &ms.ready {
            let track = item.track_label.as_deref().unwrap_or(&item.track);
            let wave = item
                .wave
                .as_deref()
                .map(|w| format!(" {}", self.dim(&format!("[{w}]"))))
                .unwrap_or_default();
            let role_style = match item.role {
                TrackRole::Critical => BOLD,
                TrackRole::Sideband => CYAN,
            };
            self.lines.push(format!(
                "    {} [{}] {}:{}{wave}  {}",
                self.look.paint(GREEN, "○"),
                self.look.paint(role_style, item.role.as_str()),
                track,
                self.look.paint(BOLD, &item.item),
                item.title
            ));
        }
    }

    fn matrix_table(&mut self, ms: &MilestoneReport) {
        let mut columns: Vec<Option<String>> =
            ms.waves.iter().map(|w| Some(w.id.clone())).collect();
        let has_unassigned = ms.tracks.iter().any(|t| {
            t.waves
                .iter()
                .any(|c| c.wave.is_none() && c.counts.items > 0)
        });
        if has_unassigned {
            columns.push(None);
        }
        if columns.is_empty() {
            for track in &ms.tracks {
                self.track_line(track);
            }
            return;
        }
        fn col_label(col: &Option<String>) -> &str {
            col.as_deref().unwrap_or("(unassigned)")
        }
        let name_w = ms
            .tracks
            .iter()
            .map(|t| {
                format!("{} [{}]", t.display_name(), t.role.as_str())
                    .chars()
                    .count()
            })
            .max()
            .unwrap_or(5)
            .max(5);
        let mut col_w: Vec<usize> = columns
            .iter()
            .map(|c| col_label(c).chars().count().max(5))
            .collect();
        for track in &ms.tracks {
            for (idx, col) in columns.iter().enumerate() {
                let cell = track.waves.iter().find(|c| c.wave == *col);
                col_w[idx] = col_w[idx].max(raw_cell(cell).chars().count());
            }
        }
        let mut header = format!("  {:<name_w$}", "Track");
        for (col, w) in columns.iter().zip(&col_w) {
            header.push_str(&format!("  {:>w$}", col_label(col)));
        }
        self.lines.push(self.dim(&header));

        for track in &ms.tracks {
            let plain_label = format!("{} [{}]", track.display_name(), track.role.as_str());
            let pad = " ".repeat(name_w.saturating_sub(plain_label.chars().count()));
            let styled_label = match track.role {
                TrackRole::Critical => format!(
                    "{} [{}]{pad}",
                    self.look.paint(BOLD, &track.display_name()),
                    track.role.as_str()
                ),
                TrackRole::Sideband => format!(
                    "{} [{}]{pad}",
                    self.look.paint(CYAN, &track.display_name()),
                    self.dim(track.role.as_str())
                ),
            };
            if let Some(reason) = &track.unavailable {
                self.lines.push(format!(
                    "  {styled_label}  {} {}",
                    self.look.paint(RED, "unavailable"),
                    self.dim(&format!("({reason})"))
                ));
                continue;
            }
            let mut line = format!("  {styled_label}");
            for (col, w) in columns.iter().zip(&col_w) {
                let cell = track.waves.iter().find(|c| c.wave == *col);
                let text = raw_cell(cell);
                let cpad = " ".repeat(w.saturating_sub(text.chars().count()));
                line.push_str("  ");
                line.push_str(&cpad);
                line.push_str(&self.paint_cell(cell, &text));
            }
            self.lines.push(line);
        }
    }

    fn track_line(&mut self, track: &TrackReport) {
        let label = track.display_name();
        let role = track.role.as_str();
        match (&track.unavailable, &track.counts) {
            (Some(reason), _) => self.lines.push(format!(
                "  {label} [{role}] {} {}",
                self.look.paint(RED, "unavailable"),
                self.dim(&format!("({reason})"))
            )),
            (None, Some(counts)) => self
                .lines
                .push(format!("  {label} [{role}]  {}", self.counts_text(counts))),
            (None, None) => self.lines.push(format!("  {label} [{role}]")),
        }
    }

    fn paint_cell(&self, cell: Option<&WaveCell>, text: &str) -> String {
        let Some(cell) = cell else {
            return self.dim(text);
        };
        if cell.counts.items == 0 {
            self.dim(text)
        } else if cell.counts.complete == cell.counts.items {
            self.look.paint(GREEN, text)
        } else if cell.counts.in_flight > 0 {
            self.look.paint(YELLOW, text)
        } else if cell.counts.blocked > 0 && cell.counts.ready == 0 {
            self.dim(text)
        } else {
            text.to_string()
        }
    }

    fn bar(&self, counts: &MatrixCounts) -> String {
        let rest = counts
            .items
            .saturating_sub(counts.complete + counts.in_flight);
        let [complete, in_flight, open] = cells([counts.complete, counts.in_flight, rest], BAR);
        format!(
            "{}{}{}",
            self.look.paint(GREEN, &"█".repeat(complete)),
            self.look.paint(YELLOW, &"▒".repeat(in_flight)),
            self.dim(&"░".repeat(open))
        )
    }

    fn counts_text(&self, counts: &MatrixCounts) -> String {
        let mut head = format!("{} of {} complete", counts.complete, counts.items);
        if let Some(pct) = counts.percent {
            head.push_str(&format!(" ({pct}%)"));
        }
        let mut parts = vec![if counts.items > 0 && counts.complete == counts.items {
            self.look.paint(GREEN, &head)
        } else {
            head
        }];
        if counts.in_flight > 0 {
            parts.push(
                self.look
                    .paint(YELLOW, &format!("{} in flight", counts.in_flight)),
            );
        }
        if counts.ready > 0 {
            parts.push(self.look.paint(GREEN, &format!("{} ready", counts.ready)));
        }
        if counts.blocked > 0 {
            parts.push(self.dim(&format!("{} blocked", counts.blocked)));
        }
        if counts.claimed > 0 {
            parts.push(
                self.look
                    .paint(CYAN, &format!("{} claimed", counts.claimed)),
            );
        }
        if counts.waiting > 0 {
            parts.push(
                self.look
                    .paint(YELLOW, &format!("{} waiting", counts.waiting)),
            );
        }
        if counts.closed > 0 {
            parts.push(self.look.paint(RED, &format!("{} closed", counts.closed)));
        }
        parts.join(&self.dim("; "))
    }

    fn dim(&self, text: &str) -> String {
        self.look.paint(DIM, text)
    }

    fn blank(&mut self) {
        if self.lines.last().is_some_and(|line| !line.is_empty()) {
            self.lines.push(String::new());
        }
    }

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

fn raw_cell(cell: Option<&WaveCell>) -> String {
    let Some(cell) = cell else {
        return "-".to_string();
    };
    if cell.counts.items == 0 {
        return "-".to_string();
    }
    let mut out = format!("{}/{}", cell.counts.complete, cell.counts.items);
    if cell.counts.in_flight > 0 {
        out.push('*');
    }
    if cell.counts.blocked > 0 {
        out.push('!');
    }
    out
}

fn topic(file: &str) -> String {
    crate::journal::parse_artifact_name(file)
        .map(|(_, topic, _)| topic)
        .unwrap_or_else(|| file.to_string())
}

fn cells(parts: [usize; 3], width: usize) -> [usize; 3] {
    let total: usize = parts.iter().sum();
    if total == 0 {
        return [0, 0, width];
    }
    let mut taken = parts.map(|part| part * width / total);
    let mut by_remainder = [0, 1, 2];
    by_remainder.sort_by_key(|&part| std::cmp::Reverse(parts[part] * width % total));
    let spare = width - taken.iter().sum::<usize>();
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
    use crate::commands::milestone::matrix::{MilestoneCounts, ReadyItem, WaveReport};

    fn strip_ansi(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' && chars.peek() == Some(&'[') {
                chars.next();
                for ch in chars.by_ref() {
                    if ch == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    fn sample_report() -> MilestoneReport {
        let w0_crit = MatrixCounts {
            items: 2,
            complete: 2,
            integrated: 2,
            percent: Some(100),
            ..Default::default()
        };
        let w1_crit = MatrixCounts {
            items: 2,
            complete: 0,
            in_flight: 1,
            blocked: 1,
            percent: Some(0),
            ..Default::default()
        };
        let sideband_counts = MatrixCounts {
            items: 1,
            complete: 0,
            open: 1,
            ready: 1,
            percent: Some(0),
            ..Default::default()
        };
        MilestoneReport {
            file: "20261123T120000Z-rc3-milestone.md".to_string(),
            title: "Release Candidate 3".to_string(),
            follows: Some("20261123T110000Z-roadmap-plan.md".to_string()),
            advisory_waves: false,
            waves: vec![
                WaveReport {
                    id: "w0".to_string(),
                    title: Some("Baseline".to_string()),
                    gated: None,
                    effective_gated: true,
                    critical: w0_crit.clone(),
                    sideband: sideband_counts.clone(),
                },
                WaveReport {
                    id: "w1".to_string(),
                    title: Some("Cuts".to_string()),
                    gated: None,
                    effective_gated: true,
                    critical: w1_crit.clone(),
                    sideband: MatrixCounts::default(),
                },
            ],
            tracks: vec![
                TrackReport {
                    project: None,
                    project_label: None,
                    file: "20261123T120000Z-core-progress.md".to_string(),
                    role: TrackRole::Critical,
                    label: Some("core".to_string()),
                    title: Some("Core".to_string()),
                    retired: None,
                    unavailable: None,
                    counts: Some(MatrixCounts {
                        items: 4,
                        complete: 2,
                        integrated: 2,
                        in_flight: 1,
                        blocked: 1,
                        percent: Some(50),
                        ..Default::default()
                    }),
                    waves: vec![
                        WaveCell {
                            wave: Some("w0".to_string()),
                            declared: true,
                            counts: w0_crit,
                            items: vec!["i1".into(), "i2".into()],
                        },
                        WaveCell {
                            wave: Some("w1".to_string()),
                            declared: true,
                            counts: w1_crit,
                            items: vec!["i3".into(), "i4".into()],
                        },
                    ],
                    items: Vec::new(),
                },
                TrackReport {
                    project: Some("other".to_string()),
                    project_label: Some("other".to_string()),
                    file: "20261123T120000Z-ui-progress.md".to_string(),
                    role: TrackRole::Critical,
                    label: Some("ui".to_string()),
                    title: None,
                    retired: None,
                    unavailable: Some("journal unreadable".to_string()),
                    counts: None,
                    waves: Vec::new(),
                    items: Vec::new(),
                },
                TrackReport {
                    project: None,
                    project_label: None,
                    file: "20261123T120000Z-cleanup-progress.md".to_string(),
                    role: TrackRole::Sideband,
                    label: Some("cleanup".to_string()),
                    title: Some("Cleanup".to_string()),
                    retired: None,
                    unavailable: None,
                    counts: Some(sideband_counts.clone()),
                    waves: vec![WaveCell {
                        wave: Some("w0".to_string()),
                        declared: true,
                        counts: sideband_counts.clone(),
                        items: vec!["i1".into()],
                    }],
                    items: Vec::new(),
                },
            ],
            counts: MilestoneCounts {
                critical: MatrixCounts {
                    items: 4,
                    complete: 2,
                    integrated: 2,
                    in_flight: 1,
                    blocked: 1,
                    percent: Some(50),
                    ..Default::default()
                },
                sideband: sideband_counts,
                unavailable_tracks: 1,
            },
            ready: vec![ReadyItem {
                track: "20261123T120000Z-cleanup-progress.md".to_string(),
                track_label: Some("cleanup".to_string()),
                role: TrackRole::Sideband,
                project: None,
                wave: Some("w0".to_string()),
                item: "i1".to_string(),
                title: "Tidy logs".to_string(),
            }],
            checkpoints: Vec::new(),
            closure: None,
        }
    }

    /// Claim: painted terminal output adds only ANSI SGR sequences around the
    /// exact same text as unpainted output. Rejects layout drift or dropped
    /// content between painted and unpainted modes.
    #[test]
    fn colour_only_paints_the_text() {
        let now: DateTime<Utc> = "2026-11-23T12:30:00Z".parse().unwrap();
        let report = sample_report();
        for ready_only in [false, true] {
            let mut plain = View::new(
                Look {
                    color: false,
                    width: None,
                },
                now,
            );
            plain.milestones(std::slice::from_ref(&report), ready_only);
            let unpainted = plain.finish();
            assert!(!unpainted.contains('\x1b'));

            let mut painted = View::new(
                Look {
                    color: true,
                    width: None,
                },
                now,
            );
            painted.milestones(std::slice::from_ref(&report), ready_only);
            let colored = painted.finish();
            assert!(colored.contains('\x1b'));
            assert_eq!(strip_ansi(&colored), unpainted);
        }
    }

    /// Claim: the matrix separates critical progress bar from sideband counts,
    /// aligns declared wave columns with cell markers, and renders unavailable
    /// tracks without dropping readable ones.
    #[test]
    fn matrix_separates_critical_sideband_and_aligns_waves() {
        let now: DateTime<Utc> = "2026-11-23T12:30:00Z".parse().unwrap();
        let report = sample_report();
        let mut view = View::new(
            Look {
                color: false,
                width: None,
            },
            now,
        );
        view.milestones(std::slice::from_ref(&report), false);
        let out = view.finish();
        assert!(out.contains("Release Candidate 3  rc3"), "{out}");
        assert!(out.contains("Critical: 2 of 4 complete (50%)"), "{out}");
        assert!(out.contains("Sideband: 0 of 1 complete (0%)"), "{out}");
        assert!(out.contains("Unavailable tracks: 1"), "{out}");
        assert!(out.contains("core [critical]"), "{out}");
        assert!(out.contains("2/2"), "{out}");
        assert!(out.contains("0/2*!"), "{out}");
        assert!(out.contains("ui [critical]"), "{out}");
        assert!(out.contains("unavailable (journal unreadable)"), "{out}");
        assert!(out.contains("cleanup [sideband]"), "{out}");
    }

    /// Claim: checkpoint revisions containing multibyte UTF-8 characters are
    /// abbreviated on character boundaries without panicking, and an unwaved
    /// cell renders distinctly from a declared wave named `"unassigned"`.
    #[test]
    fn multibyte_checkpoint_revision_and_unassigned_wave_render_without_panic() {
        let now: DateTime<Utc> = "2026-11-23T12:30:00Z".parse().unwrap();
        let mut report = sample_report();
        report
            .checkpoints
            .push(crate::journal::milestone::MilestoneCheckpoint {
                name: "utf8-cp".to_string(),
                revision: "éééééééééééééé".to_string(),
                event_id: "01TESTEVENT".to_string(),
            });
        report.tracks[0].waves.push(WaveCell {
            wave: None,
            declared: false,
            counts: MatrixCounts {
                items: 1,
                open: 1,
                ready: 1,
                ..Default::default()
            },
            items: vec!["i5".into()],
        });
        let mut view = View::new(
            Look {
                color: false,
                width: None,
            },
            now,
        );
        view.milestones(std::slice::from_ref(&report), false);
        let out = view.finish();
        assert!(
            out.contains("utf8-cp at éééééééééééé (event 01TESTEVENT)"),
            "{out}"
        );
        assert!(out.contains("(unassigned)"), "{out}");
    }
}
