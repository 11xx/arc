//! Pages: one journal artifact rendered as a standalone HTML page, for
//! reading away from the terminal, and the record of where a published copy
//! of it lives.
//!
//! The page is static by construction. Raw HTML in the Markdown is shown as
//! code, links keep only web and mail schemes or in-page anchors, images load
//! only over https, and nothing the renderer writes runs. Private text is
//! refused before anything is written, or replaced when the caller asks.

use super::private_text::{Finding, Scanner};
use super::{
    append_event, archive_dir, artifact_body_path, canonical_dir, check_artifact_name,
    ensure_storage_settled, entry_id_kind, heading_from_topic, is_global_dir, locate_artifact,
    lock_journal_transition, markdown_syntax_line, missing_write_target, newest_live_under,
    parse_artifact_name, pins, quoted_entry_line, read_events, read_located_body, recorded_anchor,
    recorded_entry, resolve, resolve_dir, valid_topic, AddressJournal, Amendments,
    ArtifactLocation, EntryScan, JournalAddress, JournalEvent,
};
use crate::commands::Ctx;
use anyhow::{bail, Context, Result};
use chrono::Utc;
use pulldown_cmark::{
    CodeBlockKind, CowStr, Event, HeadingLevel, LinkType, Options, Parser, Tag, TagEnd,
};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

const STYLE: &str = include_str!("page.css");

/// Appended section headings, each with the label a page shows for it.
const BLOCK_LABELS: [(&str, &str); 7] = [
    ("### Position", "Position"),
    ("### Evidence", "Evidence"),
    ("### Question", "Question"),
    ("### Answer", "Answer"),
    ("### Correction", "Correction"),
    ("### Retraction", "Retraction"),
    ("### Work checkpoint", "Work checkpoint"),
];

/// An http(s) URL with a host and no whitespace: what a `published` event
/// may record.
pub(super) fn valid_url(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    else {
        return false;
    };
    !url.chars().any(|c| c.is_whitespace() || c.is_control())
        && rest
            .split(['/', '?', '#'])
            .next()
            .is_some_and(|host| !host.is_empty())
}

/// The URL the latest `published` event records for `file`.
pub(super) fn published_url<'a>(events: &'a [JournalEvent], file: &str) -> Option<&'a str> {
    events
        .iter()
        .rev()
        .find(|event| event.event == "published" && event.file.as_deref() == Some(file))
        .and_then(|event| event.url.as_deref())
}

/// `arc journal published`: record where a rendered copy of an artifact
/// lives. The body may be hot or archived; recording the URL already in
/// force writes nothing.
pub(super) fn record_published(ctx: &Ctx, filename: &str, url: &str) -> Result<i32> {
    let (topic, _) = check_artifact_name(ctx, filename)?;
    let url = url.trim();
    if !valid_url(url) {
        bail!("--url must be an http(s) URL with a host and no whitespace, not {url:?}");
    }
    let dir = resolve_dir(&ctx.cwd)?;
    let _transition = lock_journal_transition(&dir)?;
    let events = read_events(&dir)?;
    ensure_storage_settled(&dir, &events, filename)?;
    if artifact_body_path(&dir, filename).is_none() {
        return Err(missing_write_target(&dir, filename));
    }
    if published_url(&events, filename) == Some(url) {
        println!("already recorded: {filename} at {url}");
        return Ok(0);
    }
    let mut event = JournalEvent::base(ctx, Utc::now(), &topic, "published");
    event.file = Some(filename.to_string());
    event.url = Some(url.to_string());
    append_event(ctx, &dir, &event)?;
    println!("published: {filename} at {url}");
    Ok(0)
}

/// `arc journal render`: one artifact as a standalone HTML page.
pub(super) fn render(
    ctx: &Ctx,
    argument: &str,
    out: Option<&Path>,
    strip_private: bool,
) -> Result<i32> {
    let location = locate(ctx, argument)?;
    let file = location.file.clone();
    let body = read_located_body(&location)?;
    let events = read_events(&location.hot)?;
    let amendments = Amendments::collect(&events, &file);
    let pin = pins::current_pin(&events, &file);

    let scanner = Scanner::new(local_roots(ctx, &location.hot));
    let title = amendments.title().map(str::to_string);
    let reason = pin.and_then(|pin| pin.note.clone());
    let mut findings: Vec<(String, Finding)> = scanner
        .findings(&body)
        .into_iter()
        .map(|finding| (format!("line {}", finding.line), finding))
        .collect();
    let block_titles = block_titles(&events, &file, &amendments);
    let extras = [
        ("title", title.as_deref()),
        ("pin reason", reason.as_deref()),
    ]
    .into_iter()
    .filter_map(|(name, value)| Some((name.to_string(), value?)))
    .chain(
        block_titles
            .iter()
            .map(|(id, title)| (format!("title of {id}"), title.as_str())),
    );
    for (name, value) in extras {
        findings.extend(
            scanner
                .findings(value)
                .into_iter()
                .map(|finding| (name.clone(), finding)),
        );
    }
    if !findings.is_empty() && !strip_private {
        let listed = findings
            .iter()
            .map(|(place, finding)| format!("  {place}: {} {}", finding.kind.label(), finding.text))
            .collect::<Vec<_>>()
            .join("\n");
        bail!(
            "{file} holds text a published page must not carry:\n{listed}\n\
             pass --strip-private to replace each in the page, or remove it from the \
             artifact with `arc journal redact {file}`"
        );
    }
    let clean = |text: &str| {
        if strip_private {
            scanner.strip(text)
        } else {
            text.to_string()
        }
    };
    let body = clean(&body);
    let page = Page {
        file: &file,
        title: title.map(|title| clean(&title)),
        reason: reason.map(|reason| clean(&reason)),
        block_titles: block_titles
            .into_iter()
            .map(|(id, title)| (id, clean(&title)))
            .collect(),
        label: journal_label(&location.hot),
        events: &events,
        amendments: &amendments,
        links: Links::new(ctx, &location, &events),
    };
    let (html, blocks) = page.html(&body)?;

    match out {
        Some(path) => std::fs::write(path, &html)
            .with_context(|| format!("cannot write {}", path.display()))?,
        None => print!("{html}"),
    }
    let replaced = if strip_private && !findings.is_empty() {
        format!(", {} private stretches replaced", findings.len())
    } else {
        String::new()
    };
    eprintln!(
        "rendered: {file} ({blocks} block{}{replaced}){}",
        if blocks == 1 { "" } else { "s" },
        out.map(|path| format!(" to {}", path.display()))
            .unwrap_or_default()
    );
    if let Some(url) = published_url(&events, &file) {
        eprintln!("published copy: {url}");
    }
    Ok(0)
}

/// The artifact an argument names: a filename or qualified file address,
/// or a topic, which is its newest live artifact, as `show` reads them.
fn locate(ctx: &Ctx, argument: &str) -> Result<ArtifactLocation> {
    let Some(address) = JournalAddress::parse(argument)? else {
        if valid_topic(argument) {
            if let Some(newest) = newest_live_under(ctx, argument)? {
                return locate(ctx, &newest);
            }
            bail!("no live artifact under topic {argument:?}");
        }
        bail!("{argument:?} is not a journal artifact name (<timestamp>-<topic>-<kind>.md)");
    };
    let Some(file) = address.file.as_deref().filter(|_| address.entry.is_none()) else {
        bail!("render takes an artifact, not an entry; name the file that records it");
    };
    Ok(match address.journal {
        AddressJournal::Own => locate_artifact(ctx, file)?,
        _ => {
            let (hot, foreign) = address.locate(ctx)?;
            ArtifactLocation {
                hot,
                file: file.to_string(),
                foreign,
            }
        }
    })
}

/// Directories known to be this machine's: the home, arc's data home, the
/// journal and its archive, and the project checkout.
fn local_roots(ctx: &Ctx, hot: &Path) -> Vec<PathBuf> {
    let mut roots = vec![hot.to_path_buf(), archive_dir(hot), ctx.cwd.clone()];
    roots.extend(crate::config::real_home().ok());
    roots.extend(crate::config::ai_home().ok());
    roots.extend(
        resolve(&ctx.cwd)
            .ok()
            .and_then(|resolution| resolution.anchor),
    );
    roots.extend(recorded_anchor(hot).ok().flatten().map(PathBuf::from));
    roots
}

/// The name a page shows for the journal it came from: the project
/// checkout's directory name, or the operator journal.
fn journal_label(hot: &Path) -> Option<String> {
    if is_global_dir(hot).unwrap_or(false) {
        return Some("operator journal".to_string());
    }
    let anchor = recorded_anchor(hot).ok().flatten()?;
    Path::new(&anchor)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
}

/// Each evidence block's title as corrected, by ID.
fn block_titles(
    events: &[JournalEvent],
    file: &str,
    amendments: &Amendments,
) -> Vec<(String, String)> {
    events
        .iter()
        .filter(|event| event.event == "evidence" && event.file.as_deref() == Some(file))
        .filter_map(|event| {
            let id = event.evidence_id.as_deref()?;
            let title = amendments.field(id, "title", event.title.as_deref())?;
            Some((id.to_string(), title.to_string()))
        })
        .collect()
}

/// One appended section of a body: its label, the ID token after it, the
/// rest of its heading line, and the text up to the next section.
struct Section<'a> {
    label: &'static str,
    token: &'a str,
    rest: &'a str,
    body: &'a str,
}

/// The body before the first appended section, and the sections in body
/// order. Quoted and fenced headings are prose, as every reader takes them.
fn sections<'a>(body: &'a str, events: &[JournalEvent], file: &str) -> (&'a str, Vec<Section<'a>>) {
    let mut preamble_end = body.len();
    let mut sections = Vec::new();
    let mut open: Option<(&'static str, &'a str, &'a str, usize)> = None;
    let mut scan = EntryScan::default();
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        if quoted_entry_line(line, &mut scan, events, file) {
            continue;
        }
        let Some((label, token, rest)) = block_heading(line) else {
            continue;
        };
        match open.take() {
            Some((label, token, rest, from)) => sections.push(Section {
                label,
                token,
                rest,
                body: &body[from..start],
            }),
            None => preamble_end = start,
        }
        open = Some((label, token, rest, offset));
    }
    if let Some((label, token, rest, from)) = open {
        sections.push(Section {
            label,
            token,
            rest,
            body: &body[from..],
        });
    }
    (&body[..preamble_end], sections)
}

fn block_heading(line: &str) -> Option<(&'static str, &str, &str)> {
    let line = markdown_syntax_line(line)?.trim_end();
    BLOCK_LABELS.iter().find_map(|(prefix, label)| {
        let rest = line.strip_prefix(prefix)?;
        if !(rest.is_empty() || rest.starts_with(char::is_whitespace)) {
            return None;
        }
        let rest = rest.trim_start();
        let (token, after) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        Some((*label, token, after.trim()))
    })
}

/// The text a page shows beside a block's title: its writer and time, from
/// the heading, without the title the heading repeats.
fn block_stamp(rest: &str, title: Option<&str>) -> String {
    let rest = title
        .and_then(|title| rest.strip_suffix(&format!(": {title}")))
        .unwrap_or(rest);
    rest.strip_prefix('(')
        .and_then(|inner| inner.strip_suffix(')'))
        .filter(|inner| !inner.contains(['(', ')']))
        .unwrap_or(rest)
        .to_string()
}

/// The writer and the date of a stamp, for a contents entry: `who, time`
/// becomes `who` and the time's date.
fn stamp_parts(stamp: &str) -> Vec<String> {
    let (who, time) = match stamp.rsplit_once(", ") {
        Some((who, time)) => (Some(who), time),
        None => (None, stamp),
    };
    let date = chrono::DateTime::parse_from_rfc3339(time.trim())
        .ok()
        .map(|time| time.format("%Y-%m-%d").to_string());
    match date {
        Some(date) => who.map(str::to_string).into_iter().chain([date]).collect(),
        None => Vec::new(),
    }
}

/// A position's text without its opening `Position: <stance>` line, which
/// the block's title already carries.
fn without_stance_line(body: &str) -> &str {
    let leading = body.len() - body.trim_start().len();
    let rest = &body[leading..];
    match rest.split_once('\n') {
        Some((first, after)) if first.trim().starts_with("Position:") => after,
        None if rest.trim().starts_with("Position:") => "",
        _ => body,
    }
}

/// A body's leading `# Title` line, which the page shows as its heading.
fn split_title(preamble: &str) -> (Option<&str>, &str) {
    let leading = preamble.len() - preamble.trim_start().len();
    let rest = &preamble[leading..];
    let (first, after) = rest.split_once('\n').unwrap_or((rest, ""));
    match first.strip_prefix("# ") {
        Some(title) if !title.trim().is_empty() => (Some(title.trim()), after),
        _ => (None, preamble),
    }
}

struct Page<'a> {
    file: &'a str,
    title: Option<String>,
    reason: Option<String>,
    block_titles: HashMap<String, String>,
    label: Option<String>,
    events: &'a [JournalEvent],
    amendments: &'a Amendments,
    links: Links<'a>,
}

struct TocEntry {
    level: u8,
    anchor: String,
    text: String,
    /// A block's writer and date, set smaller beside its title.
    stamp: Option<String>,
}

impl Page<'_> {
    /// The page, and how many content blocks it carries.
    fn html(mut self, body: &str) -> Result<(String, usize)> {
        let (preamble, sections) = sections(body, self.events, self.file);
        let (heading, preamble) = split_title(preamble);
        let (_, topic, kind) =
            parse_artifact_name(self.file).context("artifact names parse at this point")?;
        let title = self
            .title
            .clone()
            .or_else(|| heading.map(str::to_string))
            .unwrap_or_else(|| heading_from_topic(&topic));
        self.links.block_ids = sections
            .iter()
            .filter(|section| entry_id_kind(section.token).is_some())
            .map(|section| section.token.to_string())
            .collect();

        let mut used: BTreeSet<String> = ["top", "contents"].map(str::to_string).into();
        let mut toc = Vec::new();
        let mut main = String::new();
        let preamble_html = self.markdown(preamble, Shift::Body, &mut used, &mut toc);
        if !preamble_html.trim().is_empty() {
            main.push_str("<article class=\"body\">\n");
            main.push_str(&preamble_html);
            main.push_str("</article>\n");
        }
        let mut blocks = 0;
        for (index, section) in sections.iter().enumerate() {
            // An amendment shows where it applies: a corrected title, a
            // withdrawn block. Its own section is the audit trail `show` keeps.
            if matches!(section.label, "Correction" | "Retraction") {
                continue;
            }
            let id = entry_id_kind(section.token).map(|_| section.token);
            blocks += 1;
            let anchor = unique(
                &mut used,
                id.map(str::to_string)
                    .unwrap_or_else(|| format!("block-{}", index + 1)),
            );
            let recorded_title = id.and_then(|id| {
                self.events
                    .iter()
                    .find(|event| event.evidence_id.as_deref() == Some(id))
                    .and_then(|event| event.title.as_deref())
            });
            let stamp = block_stamp(section.rest, recorded_title);
            let block_title = id
                .and_then(|id| self.block_titles.get(id))
                .cloned()
                .unwrap_or_else(|| self.untitled(section, id));
            let retraction = id.and_then(|id| self.amendments.retraction(id));
            let parts = stamp_parts(&stamp);
            toc.push(TocEntry {
                level: 2,
                anchor: anchor.clone(),
                text: block_title.clone(),
                stamp: (!parts.is_empty()).then(|| parts.join(" · ")),
            });
            let class = section.label.to_ascii_lowercase().replace(' ', "-");
            main.push_str(&format!(
                "<section class=\"block block-{class}{}\" id=\"{}\">\n<header class=\"block-header\">\n<h2>{}</h2>\n<p class=\"block-meta\"><span class=\"block-kind\">{}</span>{} <span class=\"block-stamp\">{}</span></p>\n",
                if retraction.is_some() { " retracted" } else { "" },
                escape(&anchor),
                escape(&block_title),
                escape(section.label),
                id.map(|id| format!(" <code class=\"block-id\">{}</code>", escape(id)))
                    .unwrap_or_default(),
                escape(&stamp),
            ));
            if let Some(reason) = retraction {
                main.push_str(&format!(
                    "<p class=\"block-retraction\">Withdrawn: {}</p>\n",
                    escape(reason)
                ));
            }
            main.push_str("</header>\n");
            let text = if block_title.starts_with("Position:") {
                without_stance_line(section.body)
            } else {
                section.body
            };
            main.push_str(&self.markdown(text, Shift::Block, &mut used, &mut toc));
            main.push_str("</section>\n");
        }

        let mut kicker: Vec<String> = self.label.iter().cloned().collect();
        kicker.push(kind.clone());
        let mut meta = Vec::new();
        if let Some(filed) = filed_date(self.file) {
            meta.push(format!("Filed {filed}"));
        }
        meta.push(format!(
            "{blocks} appended block{}",
            if blocks == 1 { "" } else { "s" }
        ));
        if let Some(updated) = self.last_update() {
            meta.push(format!("Updated {updated}"));
        }

        let mut html = String::new();
        html.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
        html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
        html.push_str("<meta name=\"color-scheme\" content=\"light dark\">\n");
        html.push_str("<meta name=\"generator\" content=\"arc\">\n");
        html.push_str(&format!("<title>{}</title>\n", escape(&title)));
        html.push_str(&format!("<style>\n{STYLE}</style>\n</head>\n<body>\n"));
        html.push_str("<header class=\"page-header\" id=\"top\">\n");
        html.push_str(&format!(
            "<p class=\"kicker\">{}</p>\n",
            escape(&kicker.join(" · "))
        ));
        html.push_str(&format!("<h1>{}</h1>\n", escape(&title)));
        html.push_str(&format!(
            "<p class=\"meta\">{}</p>\n",
            escape(&meta.join(" · "))
        ));
        if let Some(reason) = &self.reason {
            html.push_str(&format!("<p class=\"pin\">{}</p>\n", escape(reason)));
        }
        html.push_str("</header>\n");
        html.push_str(&contents(&toc));
        html.push_str("<main>\n");
        html.push_str(&main);
        html.push_str("</main>\n");
        html.push_str(&format!(
            "<footer class=\"page-footer\">\n<p>Rendered by arc from <code>{}</code> on {}.</p>\n</footer>\n</body>\n</html>\n",
            escape(self.file),
            Utc::now().format("%Y-%m-%d")
        ));
        refuse_active_content(&html)?;
        Ok((html, blocks))
    }

    /// The title of a block its writer did not name: its kind, and for a
    /// position the stance it takes.
    fn untitled(&self, section: &Section<'_>, id: Option<&str>) -> String {
        if section.label != "Position" {
            return section.label.to_string();
        }
        let recorded = id.and_then(|id| {
            self.events
                .iter()
                .find(|event| event.position_id.as_deref() == Some(id))
                .and_then(|event| event.stance.as_deref())
        });
        let written = section
            .body
            .lines()
            .find(|line| !line.trim().is_empty())
            .and_then(|line| line.trim().strip_prefix("Position:"))
            .map(str::trim);
        let stance = id
            .and_then(|id| self.amendments.field(id, "stance", recorded))
            .or(recorded)
            .or(written)
            .filter(|stance| !stance.is_empty());
        match stance {
            Some(stance) => format!("Position: {stance}"),
            None => "Position".to_string(),
        }
    }

    /// The date of the newest content block, when one was appended.
    fn last_update(&self) -> Option<String> {
        self.events
            .iter()
            .filter(|event| {
                event.file.as_deref() == Some(self.file)
                    && event.entry_identity().is_some()
                    && !matches!(event.event.as_str(), "correction" | "retraction")
            })
            .filter_map(JournalEvent::timestamp)
            .max()
            .map(|at| at.format("%Y-%m-%d").to_string())
    }

    /// Markdown as page HTML: raw HTML shown as code, unsafe links reduced
    /// to their text, headings shifted under the page's own and given
    /// anchors, and artifact addresses linked to their published copies.
    fn markdown(
        &mut self,
        text: &str,
        shift: Shift,
        used: &mut BTreeSet<String>,
        toc: &mut Vec<TocEntry>,
    ) -> String {
        let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
        let mut events: Vec<Event<'_>> = Vec::new();
        let mut kept_links: Vec<bool> = Vec::new();
        let mut kept_images: Vec<bool> = Vec::new();
        let mut heading: Option<(usize, u8, String)> = None;
        for event in Parser::new_ext(text, options) {
            let inside_link = kept_links.iter().any(|kept| *kept) || !kept_images.is_empty();
            match event {
                Event::Html(raw) => events.push(Event::Text(raw)),
                Event::InlineHtml(raw) => events.push(Event::Code(raw)),
                Event::Start(Tag::Table(alignments)) => {
                    events.push(Event::Html(CowStr::from("<div class=\"table-scroll\">\n")));
                    events.push(Event::Start(Tag::Table(alignments)));
                }
                Event::End(TagEnd::Table) => {
                    events.push(Event::End(TagEnd::Table));
                    events.push(Event::Html(CowStr::from("</div>\n")));
                }
                Event::Start(Tag::HtmlBlock) => {
                    events.push(Event::Start(Tag::CodeBlock(CodeBlockKind::Indented)));
                }
                Event::End(TagEnd::HtmlBlock) => events.push(Event::End(TagEnd::CodeBlock)),
                Event::Start(Tag::Link {
                    link_type,
                    dest_url,
                    title,
                    id,
                }) => {
                    let kept = safe_href(&dest_url) && !inside_link;
                    kept_links.push(kept);
                    if kept {
                        events.push(Event::Start(Tag::Link {
                            link_type,
                            dest_url,
                            title,
                            id,
                        }));
                    }
                }
                Event::End(TagEnd::Link) => {
                    if kept_links.pop().unwrap_or(false) {
                        events.push(Event::End(TagEnd::Link));
                    }
                }
                Event::Start(Tag::Image {
                    link_type,
                    dest_url,
                    title,
                    id,
                }) => {
                    let kept = dest_url.to_ascii_lowercase().starts_with("https://");
                    kept_images.push(kept);
                    if kept {
                        events.push(Event::Start(Tag::Image {
                            link_type,
                            dest_url,
                            title,
                            id,
                        }));
                    }
                }
                Event::End(TagEnd::Image) => {
                    if kept_images.pop().unwrap_or(false) {
                        events.push(Event::End(TagEnd::Image));
                    }
                }
                Event::Start(Tag::Heading { level, .. }) => {
                    let level = shift.level(level);
                    heading = Some((events.len(), level, String::new()));
                    events.push(Event::Start(Tag::Heading {
                        level: heading_level(level),
                        id: None,
                        classes: Vec::new(),
                        attrs: Vec::new(),
                    }));
                }
                Event::End(TagEnd::Heading(_)) => {
                    if let Some((index, level, text)) = heading.take() {
                        let anchor = unique(used, slug(&text));
                        events[index] = Event::Start(Tag::Heading {
                            level: heading_level(level),
                            id: Some(CowStr::from(anchor.clone())),
                            classes: Vec::new(),
                            attrs: Vec::new(),
                        });
                        if level <= 3 {
                            toc.push(TocEntry {
                                level,
                                anchor,
                                text: text.trim().to_string(),
                                stamp: None,
                            });
                        }
                        events.push(Event::End(TagEnd::Heading(heading_level(level))));
                    }
                }
                Event::Text(text) => {
                    if let Some((_, _, heading)) = heading.as_mut() {
                        heading.push_str(&text);
                    }
                    if inside_link {
                        events.push(Event::Text(text));
                    } else {
                        self.link_addresses(&text, &mut events);
                    }
                }
                Event::Code(code) => {
                    if let Some((_, _, heading)) = heading.as_mut() {
                        heading.push_str(&code);
                    }
                    match (!inside_link)
                        .then(|| self.links.href(code.trim()))
                        .flatten()
                    {
                        Some(href) => {
                            events.push(link_start(href));
                            events.push(Event::Code(code));
                            events.push(Event::End(TagEnd::Link));
                        }
                        None => events.push(Event::Code(code)),
                    }
                }
                Event::TaskListMarker(done) => {
                    events.push(Event::Text(CowStr::from(if done {
                        "[x] "
                    } else {
                        "[ ] "
                    })));
                }
                Event::InlineMath(math) | Event::DisplayMath(math) => {
                    events.push(Event::Code(math));
                }
                other => events.push(other),
            }
        }
        let mut html = String::new();
        pulldown_cmark::html::push_html(&mut html, events.into_iter());
        html
    }

    /// Text with each artifact address that resolves to a published copy
    /// wrapped in a link to it.
    fn link_addresses<'t>(&mut self, text: &CowStr<'t>, events: &mut Vec<Event<'t>>) {
        let mut cursor = 0;
        for (start, token) in address_tokens(text) {
            let Some(href) = self.links.href(token) else {
                continue;
            };
            if start > cursor {
                events.push(Event::Text(CowStr::from(text[cursor..start].to_string())));
            }
            events.push(link_start(href));
            events.push(Event::Text(CowStr::from(token.to_string())));
            events.push(Event::End(TagEnd::Link));
            cursor = start + token.len();
        }
        if cursor == 0 {
            events.push(Event::Text(text.clone()));
        } else if cursor < text.len() {
            events.push(Event::Text(CowStr::from(text[cursor..].to_string())));
        }
    }
}

#[derive(Clone, Copy)]
enum Shift {
    /// The artifact's own text: an `h1` drops to `h2` under the page title.
    Body,
    /// An appended block's text, under the block's `h2`.
    Block,
}

impl Shift {
    fn level(self, level: HeadingLevel) -> u8 {
        let level = level as u8;
        match self {
            Shift::Body => level.max(2),
            Shift::Block => (level + 1).clamp(3, 6),
        }
    }
}

fn heading_level(level: u8) -> HeadingLevel {
    match level {
        0..=1 => HeadingLevel::H1,
        2 => HeadingLevel::H2,
        3 => HeadingLevel::H3,
        4 => HeadingLevel::H4,
        5 => HeadingLevel::H5,
        _ => HeadingLevel::H6,
    }
}

fn link_start<'t>(href: String) -> Event<'t> {
    Event::Start(Tag::Link {
        link_type: LinkType::Inline,
        dest_url: CowStr::from(href),
        title: CowStr::from(""),
        id: CowStr::from(""),
    })
}

/// A link a published page may carry: the web, mail, or an in-page anchor.
fn safe_href(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    ["https://", "http://", "mailto:", "#"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
}

/// Stretches of text that may spell an artifact address, with their byte
/// offsets: a qualified address, an artifact filename, or an entry ID.
fn address_tokens(text: &str) -> Vec<(usize, &str)> {
    let member =
        |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '#' | '/');
    let mut tokens = Vec::new();
    let mut start = None;
    for (at, c) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        match (member(c), start) {
            (true, None) => start = Some(at),
            (false, Some(from)) => {
                let token = text[from..at].trim_end_matches(['.', ':', '-']);
                if token.contains("::")
                    || token.ends_with(".md")
                    || token.contains(".md#")
                    || entry_id_kind(token).is_some()
                {
                    tokens.push((from, token));
                }
                start = None;
            }
            _ => {}
        }
    }
    tokens
}

/// Where artifact addresses written in a page point: an entry of the page
/// itself to its anchor, and any other artifact or entry to the copy its
/// own journal records as published. Unresolved addresses stay text.
struct Links<'a> {
    ctx: &'a Ctx,
    hot: PathBuf,
    file: String,
    events: &'a [JournalEvent],
    block_ids: BTreeSet<String>,
    journals: HashMap<PathBuf, Vec<JournalEvent>>,
}

impl<'a> Links<'a> {
    fn new(ctx: &'a Ctx, location: &ArtifactLocation, events: &'a [JournalEvent]) -> Self {
        Self {
            ctx,
            hot: canonical_dir(&location.hot),
            file: location.file.clone(),
            events,
            block_ids: BTreeSet::new(),
            journals: HashMap::new(),
        }
    }

    fn href(&mut self, token: &str) -> Option<String> {
        let address = JournalAddress::parse(token).ok().flatten()?;
        let hot = match &address.journal {
            AddressJournal::Own => self.hot.clone(),
            _ => canonical_dir(&address.locate(self.ctx).ok()?.0),
        };
        let own = hot == self.hot;
        if own {
            if let Some(id) = address.entry.as_deref() {
                let here = address.file.as_deref().is_none_or(|file| file == self.file);
                if here && self.block_ids.contains(id) {
                    return Some(format!("#{id}"));
                }
            }
        } else if !self.journals.contains_key(&hot) {
            let events = read_events(&hot).unwrap_or_default();
            self.journals.insert(hot.clone(), events);
        }
        let events: &[JournalEvent] = if own {
            self.events
        } else {
            self.journals.get(&hot)?
        };
        let file = match (address.file.as_deref(), address.entry.as_deref()) {
            (Some(file), _) => file.to_string(),
            (None, Some(id)) => recorded_entry(events, id)?.file.clone()?,
            (None, None) => return None,
        };
        if own && file == self.file {
            return None;
        }
        let url = published_url(events, &file)?;
        Some(match address.entry.as_deref() {
            Some(id) => format!("{}#{id}", url.trim_end_matches('#')),
            None => url.to_string(),
        })
    }
}

fn contents(toc: &[TocEntry]) -> String {
    if toc.len() < 2 {
        return String::new();
    }
    let mut html = String::from(
        "<nav class=\"contents\" id=\"contents\" aria-label=\"Contents\">\n<h2>Contents</h2>\n<ol>\n",
    );
    let mut item_open = false;
    let mut nested = false;
    for entry in toc {
        let link = format!(
            "<a href=\"#{}\">{}{}</a>",
            escape(&entry.anchor),
            escape(&entry.text),
            entry
                .stamp
                .as_deref()
                .map(|stamp| format!(" <span class=\"toc-stamp\">{}</span>", escape(stamp)))
                .unwrap_or_default()
        );
        if entry.level > 2 && item_open {
            if !nested {
                html.push_str("\n<ol>\n");
                nested = true;
            }
            html.push_str(&format!("<li>{link}</li>\n"));
            continue;
        }
        if nested {
            html.push_str("</ol>\n");
            nested = false;
        }
        if item_open {
            html.push_str("</li>\n");
        }
        html.push_str(&format!("<li>{link}"));
        item_open = true;
    }
    if nested {
        html.push_str("</ol>\n");
    }
    if item_open {
        html.push_str("</li>\n");
    }
    html.push_str("</ol>\n</nav>\n");
    html
}

fn filed_date(file: &str) -> Option<String> {
    let (stamp, _, _) = parse_artifact_name(file)?;
    let date = stamp.get(..8)?;
    Some(format!("{}-{}-{}", &date[..4], &date[4..6], &date[6..8]))
}

fn slug(text: &str) -> String {
    let mut slug = String::new();
    for c in text.trim().chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-').to_string();
    if slug.is_empty() {
        "section".to_string()
    } else {
        slug
    }
}

fn unique(used: &mut BTreeSet<String>, wanted: String) -> String {
    let mut candidate = wanted.clone();
    let mut n = 2;
    while used.contains(&candidate) {
        candidate = format!("{wanted}-{n}");
        n += 1;
    }
    used.insert(candidate.clone());
    candidate
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// The last guard on a page: an element that runs, embeds, submits or
/// redirects. Nothing the renderer emits is one, so finding one is a defect
/// in the renderer, never a page to write.
fn refuse_active_content(html: &str) -> Result<()> {
    let lower = html.to_ascii_lowercase();
    for element in [
        "<script",
        "<iframe",
        "<form",
        "<object",
        "<embed",
        "<input",
        "http-equiv",
    ] {
        if lower.contains(element) {
            bail!("the rendered page carries `{element}`; refusing to write it");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_need_a_web_scheme_and_a_host() {
        assert!(valid_url("https://notes.example.dev/"));
        assert!(valid_url("http://example.com/a?b#c"));
        for bad in [
            "ftp://x",
            "https://",
            "https:///x",
            "https://a b",
            "javascript:alert(1)",
        ] {
            assert!(!valid_url(bad), "{bad}");
        }
    }

    #[test]
    fn stamps_drop_the_parentheses_and_the_repeated_title() {
        assert_eq!(
            block_stamp(
                "(model via harness, 2026-11-09T01:00:00Z): Findings",
                Some("Findings")
            ),
            "model via harness, 2026-11-09T01:00:00Z"
        );
        assert_eq!(
            block_stamp("(2026-11-09T01:00:00Z)", None),
            "2026-11-09T01:00:00Z"
        );
        assert_eq!(block_stamp("= yes (a, t)", None), "= yes (a, t)");
    }

    #[test]
    fn address_tokens_find_qualified_files_names_and_ids() {
        let text = "See tray::20261020T000000Z-levels-note.md, then evd-01m6z103q4jkfw3q8kwe7fvqh2. Also x.md#q-1.";
        let tokens: Vec<_> = address_tokens(text).into_iter().map(|(_, t)| t).collect();
        assert_eq!(
            tokens,
            vec![
                "tray::20261020T000000Z-levels-note.md",
                "evd-01m6z103q4jkfw3q8kwe7fvqh2",
                "x.md#q-1"
            ]
        );
    }

    #[test]
    fn contents_nest_subheadings_under_their_section() {
        let entry = |level, anchor: &str| TocEntry {
            level,
            anchor: anchor.to_string(),
            text: anchor.to_string(),
            stamp: None,
        };
        let html = contents(&[entry(2, "a"), entry(3, "a1"), entry(3, "a2"), entry(2, "b")]);
        assert_eq!(
            html,
            "<nav class=\"contents\" id=\"contents\" aria-label=\"Contents\">\n<h2>Contents</h2>\n<ol>\n\
             <li><a href=\"#a\">a</a>\n<ol>\n<li><a href=\"#a1\">a1</a></li>\n<li><a href=\"#a2\">a2</a></li>\n</ol>\n</li>\n\
             <li><a href=\"#b\">b</a></li>\n</ol>\n</nav>\n"
        );
    }
}
