//! Commit message trailers: the `Key: value` lines a message ends with, and
//! editing them without touching anything else the message says.
//!
//! A message is bytes. It is not required to be UTF-8 — that is what a
//! commit's `encoding` header is for — so the structure is read from ASCII
//! alone and every byte outside an edited line travels through untouched.
//!
//! The block is the message's last paragraph when every line in it is a
//! trailer or a continuation of one, and never the first line, which is the
//! subject: a one-line message whose subject happens to be spelled
//! `feat: something` has no trailers, and treating it as a block would append
//! to the subject.
//!
//! [`check_contribution`] reads the same block against the portable
//! contribution-trailer convention without editing anything.

/// The role keys the convention defines: `Planned-by`, `Implemented-by`,
/// `Reviewed-by`, `Orchestrated-by`, in canonical order.
const CONTRIBUTION_ROLES: [&str; 4] = [
    "Planned-by",
    "Implemented-by",
    "Reviewed-by",
    "Orchestrated-by",
];

/// A disclosure key the convention retains as written rather than promoting
/// to a role, because older commits carry it and its role is not recorded.
const LEGACY_DISCLOSURE: &str = "Assisted-by";

/// One thing a contribution check found in a message. `kind` separates a
/// value a role key cannot read, a readable value that is no fresh
/// coordinate, and a key outside the convention; none changes the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContributionIssue {
    pub kind: &'static str,
    pub key: String,
    pub value: String,
    pub detail: &'static str,
}

impl ContributionIssue {
    /// The report line a caller prints.
    pub fn line(&self) -> String {
        format!(
            "{}: {}: {} ({})",
            self.kind, self.key, self.value, self.detail
        )
    }
}

/// Whether a role key's value is one of the convention's forms: an agent
/// `harness:model[#effort]` coordinate, or a human `Name <email>` value. A
/// value is one line, and no coordinate is empty.
pub(crate) fn value_is_readable(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() || value.contains(['\n', '\r']) {
        return false;
    }
    if let Some((name, address)) = value.rsplit_once('<') {
        return !name.trim().is_empty()
            && address.ends_with('>')
            && !address.trim_end_matches('>').trim().is_empty();
    }
    let Some((harness, rest)) = value.split_once(':') else {
        return false;
    };
    let (model, effort) = match rest.split_once('#') {
        Some((model, effort)) => (model, Some(effort)),
        None => (rest, None),
    };
    !harness.trim().is_empty()
        && !model.trim().is_empty()
        && effort.is_none_or(|effort| !effort.trim().is_empty())
}

/// Every contribution-trailer issue in a message's trailer block, in the
/// order the lines appear. The message is read, never written: the answer is
/// a report, not an edit.
///
/// A role key with a value the convention cannot read is `malformed`, and one
/// whose readable value carries a placeholder or a session id, or is a
/// `Name <address>` without an email, is `placeholder`: a coordinate nobody
/// established is omitted, never written. A key outside the convention is
/// `unsupported` and travels through untouched; [`LEGACY_DISCLOSURE`] is
/// defined, so an older `Assisted-by` is reported as none of these.
pub fn check_contribution(message: &[u8]) -> Vec<ContributionIssue> {
    let ends_with_newline = message.ends_with(b"\n");
    let body = match ends_with_newline {
        true => &message[..message.len() - 1],
        false => message,
    };
    let lines: Vec<&[u8]> = match body.is_empty() {
        true => Vec::new(),
        false => body.split(|byte| *byte == b'\n').collect(),
    };
    let Some(block) = block_of(&lines) else {
        return Vec::new();
    };
    lines[block]
        .iter()
        .filter_map(|line| line_issue(line, None))
        .collect()
}

/// The first of `lines` whose role key carries a value that is no fresh
/// coordinate: a line [`check_contribution`] reports as `malformed` or
/// `placeholder`, and so a line no write records.
pub(crate) fn first_unrecordable_role(
    lines: &[String],
    session: Option<&str>,
) -> Option<ContributionIssue> {
    lines
        .iter()
        .filter_map(|line| line_issue(line.as_bytes(), session))
        .find(|issue| issue.kind != "unsupported")
}

/// What the convention says about one trailer line, if anything.
fn line_issue(line: &[u8], session: Option<&str>) -> Option<ContributionIssue> {
    let key = trailer_key(line)?;
    let key = String::from_utf8_lossy(&key).into_owned();
    let value = String::from_utf8_lossy(&line[key.len() + 1..])
        .trim()
        .to_string();
    if CONTRIBUTION_ROLES
        .iter()
        .any(|role| role.eq_ignore_ascii_case(&key))
    {
        let defect = crate::coordinate::coordinate_defect(&value, session)?;
        let kind = match defect {
            crate::coordinate::Defect::Unreadable => "malformed",
            _ => "placeholder",
        };
        Some(ContributionIssue {
            kind,
            key,
            value,
            detail: defect.reason(),
        })
    } else if !key.eq_ignore_ascii_case(LEGACY_DISCLOSURE) {
        Some(ContributionIssue {
            kind: "unsupported",
            key,
            value,
            detail: "outside arc-contribution-trailers/2; retained untouched",
        })
    } else {
        None
    }
}

/// A message cut into lines, with where its trailer block sits.
struct Parts<'a> {
    lines: Vec<&'a [u8]>,
    block: Option<std::ops::Range<usize>>,
    ends_with_newline: bool,
}

impl<'a> Parts<'a> {
    fn of(message: &'a [u8]) -> Self {
        let ends_with_newline = message.ends_with(b"\n");
        let body = match ends_with_newline {
            true => &message[..message.len() - 1],
            false => message,
        };
        let lines: Vec<&[u8]> = match body.is_empty() {
            true => Vec::new(),
            false => body.split(|byte| *byte == b'\n').collect(),
        };
        let block = block_of(&lines);
        Parts {
            lines,
            block,
            ends_with_newline,
        }
    }

    /// The message holding `kept` as its trailer block, and every other line
    /// as it was. It equals `original` exactly when the block is unchanged.
    fn assemble(&self, original: &[u8], kept: Vec<Vec<u8>>) -> Vec<u8> {
        let (before, after) = match &self.block {
            Some(block) => (&self.lines[..block.start], &self.lines[block.end..]),
            // A message with no block grows one after everything it says, with
            // the blank line that separates a paragraph.
            None => (&self.lines[..], &self.lines[self.lines.len()..]),
        };
        if self.block.is_none() && kept.is_empty() {
            return original.to_vec();
        }

        let mut out: Vec<Vec<u8>> = before.iter().map(|line| line.to_vec()).collect();
        if kept.is_empty() {
            // Dropping every trailer leaves an empty paragraph, and the blank
            // line that opened it separates nothing.
            while out.last().is_some_and(|line| is_blank(line)) {
                out.pop();
            }
        } else {
            if self.block.is_none() && out.last().is_some_and(|line| !is_blank(line)) {
                out.push(Vec::new());
            }
            out.extend(kept);
        }
        out.extend(after.iter().map(|line| line.to_vec()));

        let mut edited = out.join(&b'\n');
        if self.ends_with_newline && !edited.is_empty() {
            edited.push(b'\n');
        }
        edited
    }
}

/// Edit a message's trailers: drop every trailer whose key is one of `drop`,
/// then add each line of `append` the block does not already carry verbatim.
///
/// The answer is the message the commit is to hold. It equals the original
/// exactly when the edit had nothing to do, which is how a caller tells a
/// commit it must recreate from one it must leave alone.
pub fn edit(message: &[u8], drop: &[String], append: &[String]) -> Vec<u8> {
    let parts = Parts::of(message);

    let mut kept: Vec<Vec<u8>> = Vec::new();
    if let Some(block) = &parts.block {
        // A continuation line says nothing on its own, so it goes wherever
        // the trailer above it went.
        let mut dropping = false;
        for line in &parts.lines[block.clone()] {
            if let Some(key) = trailer_key(line) {
                dropping = drop.iter().any(|wanted| same_key(&key, wanted));
            }
            if !dropping {
                kept.push(line.to_vec());
            }
        }
    }
    for line in append {
        let line = line.as_bytes();
        if !kept.iter().any(|have| have == line) {
            kept.push(line.to_vec());
        }
    }
    parts.assemble(message, kept)
}

/// The version of the per-line edit file format [`parse_edits`] reads.
pub const EDITS_SCHEMA: &str = "arc-trailer-edits/1";

/// What an edit does to the line it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditAction {
    Replace,
    Drop,
}

impl EditAction {
    pub fn as_str(self) -> &'static str {
        match self {
            EditAction::Replace => "replace",
            EditAction::Drop => "drop",
        }
    }
}

/// One edit of one trailer line of one commit.
#[derive(Debug, Clone)]
pub struct LineEdit {
    /// Where the edit was written, as `file:line`, for the refusals that
    /// concern it.
    pub origin: String,
    /// The full commit id the edit applies to, in lower case.
    pub commit: String,
    pub action: EditAction,
    /// The trailer line to find, as a commit carries it.
    pub old: String,
    /// The line that takes its place; empty for a drop.
    pub new: String,
}

/// Read an `arc-trailer-edits/1` file: one tab-separated edit per line, as
/// `<commit>`, `replace` or `drop`, the old trailer line and the new one.
///
/// Blank lines and `#` comments are skipped, and a comment naming another
/// `arc-trailer-edits` version is refused. A drop may leave out its empty last
/// field, which editors strip. A field beginning with a double quote is a
/// JSON string: escapes encode tabs, quotes and backslashes. Unquoted fields
/// keep literal backslashes. Quoted fields contain no carriage return or
/// newline. Every refusal names `source` and the line. Replacement models
/// must not equal `session` when the caller establishes it.
pub fn parse_edits(
    text: &str,
    source: &str,
    session: Option<&str>,
) -> anyhow::Result<Vec<LineEdit>> {
    let mut edits = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let origin = format!("{source}:{}", index + 1);
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(comment) = line.strip_prefix('#') {
            let comment = comment.trim();
            if comment.starts_with("arc-trailer-edits/") && comment != EDITS_SCHEMA {
                anyhow::bail!("{origin}: this file is {comment}; this arc reads {EDITS_SCHEMA}");
            }
            continue;
        }
        let fields: Vec<String> = raw
            .split('\t')
            .map(|field| {
                let field = field.trim();
                if field.starts_with('"') {
                    let decoded = serde_json::from_str::<String>(field).map_err(|error| {
                        anyhow::anyhow!("{origin}: invalid JSON-quoted field: {error}")
                    })?;
                    if decoded.contains(['\n', '\r']) {
                        anyhow::bail!("{origin}: quoted fields contain one line, without carriage returns or newlines");
                    }
                    Ok(decoded)
                } else {
                    Ok(field.to_string())
                }
            })
            .collect::<anyhow::Result<_>>()?;
        let fields: Vec<&str> = fields.iter().map(String::as_str).collect();
        let (commit, action, old, new) = match fields[..] {
            [commit, action, old] => (commit, action, old, ""),
            [commit, action, old, new] => (commit, action, old, new),
            _ => anyhow::bail!(
                "{origin}: expected <commit>, replace|drop, the old line and the new line \
                 separated by tabs, found {} fields",
                fields.len()
            ),
        };
        if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            anyhow::bail!("{origin}: {commit:?} is not a full commit sha");
        }
        let action = match action {
            "replace" => EditAction::Replace,
            "drop" => EditAction::Drop,
            other => anyhow::bail!("{origin}: the action is replace or drop, not {other:?}"),
        };
        if trailer_key(old.as_bytes()).is_none() {
            anyhow::bail!("{origin}: the old line {old:?} is not a `Key: value` trailer line");
        }
        match action {
            EditAction::Drop if !new.is_empty() => {
                anyhow::bail!("{origin}: a drop names no new line, found {new:?}")
            }
            EditAction::Replace if new.is_empty() => {
                anyhow::bail!("{origin}: a replace needs the new line; use drop to remove one")
            }
            EditAction::Replace => {
                if trailer_key(new.as_bytes()).is_none() {
                    anyhow::bail!(
                        "{origin}: the new line {new:?} is not a `Key: value` trailer line"
                    );
                }
                if let Some(issue) = unrecordable_line(new, session) {
                    anyhow::bail!(
                        "{origin}: the new line {new:?} is no fresh coordinate: {}; write a \
                         fresh coordinate or drop the line",
                        issue
                    );
                }
            }
            EditAction::Drop => {}
        }
        edits.push(LineEdit {
            origin,
            commit: commit.to_ascii_lowercase(),
            action,
            old: old.to_string(),
            new: new.to_string(),
        });
    }
    Ok(edits)
}

/// Why a trailer line a write would record is no fresh coordinate, when its
/// key says it holds one: a role key's value, or an `Assisted-by` value read
/// without its trailing `(role)`.
fn unrecordable_line(line: &str, session: Option<&str>) -> Option<&'static str> {
    let key = String::from_utf8_lossy(&trailer_key(line.as_bytes())?).into_owned();
    let value = line[key.len() + 1..].trim();
    let value = if key.eq_ignore_ascii_case(LEGACY_DISCLOSURE) {
        match value.rsplit_once(" (") {
            Some((coordinate, role)) if role.ends_with(')') => coordinate.trim(),
            _ => value,
        }
    } else if CONTRIBUTION_ROLES
        .iter()
        .any(|role| role.eq_ignore_ascii_case(&key))
    {
        value
    } else {
        return None;
    };
    crate::coordinate::coordinate_defect(value, session).map(crate::coordinate::Defect::reason)
}

/// Apply the edits that name one commit to its message, or say which edit
/// cannot be applied.
///
/// Every edit reads the block as the commit carries it, so the order of the
/// edits does not matter. An old line has to be one trailer line of the block,
/// once, and not the head of a value continuing below it; two edits cannot name
/// one line.
pub fn edit_lines(message: &[u8], edits: &[&LineEdit]) -> anyhow::Result<Vec<u8>> {
    let parts = Parts::of(message);
    let Some(block) = parts.block.clone() else {
        if let Some(edit) = edits.first() {
            anyhow::bail!(
                "{}: {} has no trailer block to edit",
                edit.origin,
                short(&edit.commit)
            );
        }
        return Ok(message.to_vec());
    };
    let lines = &parts.lines[block.clone()];
    let mut named: std::collections::BTreeMap<usize, &LineEdit> = Default::default();
    for edit in edits {
        let found: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| trailer_key(line).is_some() && trimmed(line) == edit.old.as_bytes())
            .map(|(index, _)| index)
            .collect();
        let index = match found[..] {
            [index] => index,
            [] => anyhow::bail!(
                "{}: {:?} is not a trailer line in the trailer block of {}",
                edit.origin,
                edit.old,
                short(&edit.commit)
            ),
            _ => anyhow::bail!(
                "{}: {:?} occurs {} times in the trailer block of {}; an edit names one line",
                edit.origin,
                edit.old,
                found.len(),
                short(&edit.commit)
            ),
        };
        if lines
            .get(index + 1)
            .is_some_and(|next| is_continuation(next))
        {
            anyhow::bail!(
                "{}: {:?} continues on the next line; an edit replaces or drops one whole line",
                edit.origin,
                edit.old
            );
        }
        if let Some(earlier) = named.insert(index, edit) {
            anyhow::bail!(
                "{}: {:?} is also edited at {}; one line takes one edit",
                edit.origin,
                edit.old,
                earlier.origin
            );
        }
    }
    let mut kept: Vec<Vec<u8>> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        match named.get(&index) {
            Some(edit) if edit.action == EditAction::Replace => {
                kept.push(edit.new.as_bytes().to_vec())
            }
            Some(_) => {}
            None => kept.push(line.to_vec()),
        }
    }
    Ok(parts.assemble(message, kept))
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(8)]
}

/// A line without the whitespace around it.
fn trimmed(line: &[u8]) -> &[u8] {
    let start = line
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(line.len());
    let end = line
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |end| end + 1);
    &line[start..end]
}

/// Which lines hold the trailer block, if the message has one.
///
/// The block is the last paragraph, taken to be trailers only when every one
/// of its lines is a trailer or a continuation. A paragraph holding one line
/// of prose is prose, whatever else it holds. The first line of the message
/// is the subject and never a trailer, so a message of a single paragraph has
/// no block.
fn block_of(lines: &[&[u8]]) -> Option<std::ops::Range<usize>> {
    let end = lines.iter().rposition(|line| !is_blank(line))? + 1;
    let start = lines[..end].iter().rposition(|line| is_blank(line))? + 1;
    let paragraph = &lines[start..end];
    let opens = trailer_key(paragraph[0]).is_some();
    let closes = paragraph[1..]
        .iter()
        .all(|line| trailer_key(line).is_some() || is_continuation(line));
    (opens && closes).then_some(start..end)
}

/// The key of a trailer line: `[A-Za-z0-9-]+` before a colon, at the start of
/// the line.
fn trailer_key(line: &[u8]) -> Option<Vec<u8>> {
    let key = line.split(|byte| *byte == b':').next()?;
    let spelled = !key.is_empty()
        && key
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-');
    (spelled && key.len() < line.len()).then(|| key.to_vec())
}

/// Whether a line continues the trailer above it, which is how a trailer
/// carries a value spanning lines.
fn is_continuation(line: &[u8]) -> bool {
    line.first().is_some_and(u8::is_ascii_whitespace)
}

fn is_blank(line: &[u8]) -> bool {
    line.iter().all(u8::is_ascii_whitespace)
}

/// Whether a key names the same trailer as one the caller asked to drop.
/// Trailer keys are matched without case, the way `git interpret-trailers`
/// matches them, so `Foo-Session` and `foo-session` are one key.
fn same_key(key: &[u8], wanted: &str) -> bool {
    key.eq_ignore_ascii_case(wanted.as_bytes())
}

/// Validate explicit roles with the same value grammar as the convention checker.
pub(crate) fn validate_roles(
    roles: &std::collections::BTreeMap<String, Vec<String>>,
) -> anyhow::Result<()> {
    for (role, values) in roles {
        if !matches!(role.as_str(), "Planned-by" | "Orchestrated-by") {
            anyhow::bail!("role {role:?} must be Planned-by or Orchestrated-by");
        }
        for value in values {
            if !value_is_readable(value) {
                anyhow::bail!("invalid {role} value: expected harness:model[#effort] or Name <email> on one line");
            }
        }
    }
    Ok(())
}

/// Whether the final trailer block carries the requested line verbatim.
pub(crate) fn contains(message: &[u8], wanted: &str) -> bool {
    let lines: Vec<&[u8]> = message.split(|byte| *byte == b'\n').collect();
    block_of(&lines).is_some_and(|block| lines[block].contains(&wanted.as_bytes()))
}

/// The values of the final trailer block's `Implemented-by` lines, in the
/// order they appear, each without the whitespace around it.
pub(crate) fn implemented_by(message: &[u8]) -> Vec<String> {
    let lines: Vec<&[u8]> = message.split(|byte| *byte == b'\n').collect();
    let Some(block) = block_of(&lines) else {
        return Vec::new();
    };
    lines[block]
        .iter()
        .filter(|line| trailer_key(line).is_some_and(|key| same_key(&key, "Implemented-by")))
        .map(|line| {
            let value = &line[line.iter().position(|byte| *byte == b':').unwrap_or(0) + 1..];
            String::from_utf8_lossy(trimmed(value)).into_owned()
        })
        .collect()
}

/// The model a role value names, without its effort: the `model` of
/// `harness:model[#effort]`. A person, an unreadable value and a placeholder
/// name none.
pub(crate) fn coordinate_model(value: &str) -> Option<&str> {
    if crate::coordinate::coordinate_defect(value, None).is_some() || value.contains('<') {
        return None;
    }
    let (_, rest) = value.trim().split_once(':')?;
    Some(rest.split_once('#').map_or(rest, |(model, _)| model).trim())
}

/// The first of `values` that names the model `model` declares, ignoring
/// effort: the model of a `harness:model[#effort]` line against the
/// `model[#effort]` a recorded identity carries.
pub(crate) fn naming_model<'a>(values: &'a [String], model: &str) -> Option<&'a str> {
    let model = model.split('#').next().unwrap_or(model).trim();
    values
        .iter()
        .find(|value| !model.is_empty() && coordinate_model(value) == Some(model))
        .map(String::as_str)
}

/// The `Implemented-by` values of every commit in `base..head`, verbatim, in
/// the order first met and without repeats.
pub(crate) fn range_implementers(
    cwd: &std::path::Path,
    base: &str,
    head: &str,
) -> anyhow::Result<Vec<String>> {
    let mut seen = Vec::<String>::new();
    for (_, message) in crate::gitio::range_messages(cwd, base, head)? {
        for value in implemented_by(&message) {
            if !seen.contains(&value) {
                seen.push(value);
            }
        }
    }
    Ok(seen)
}

/// Portable contribution lines and unresolved attribution notes for a change.
#[derive(serde::Serialize)]
pub(crate) struct ChangeTrailers {
    pub schema: &'static str,
    pub trailers: Vec<String>,
    pub notes: Vec<String>,
}

/// The trailer block a change projects, and a note for every contribution it
/// cannot attribute.
///
/// A line carries a fresh coordinate or nothing: a placeholder, a session id
/// or an unresolved identity is named in a note, with the value and why, and
/// never written as a coordinate.
pub(crate) fn for_change(state: &crate::state::ChangeState) -> ChangeTrailers {
    let mut trailers = Vec::new();
    let mut notes = Vec::new();
    for role in CONTRIBUTION_ROLES {
        if role == "Reviewed-by" {
            continue;
        }
        if role == "Implemented-by" {
            for patchset in &state.patchsets {
                let subjects = if patchset.contributors.is_empty() {
                    vec![patchset.effective_author()]
                } else {
                    patchset.contributors.iter().map(String::as_str).collect()
                };
                for subject in subjects {
                    match implementation_identity(state, patchset, subject) {
                        Ok(value) => {
                            let line = format!("{role}: {value}");
                            if !trailers.contains(&line) {
                                trailers.push(line);
                            }
                        }
                        Err(reason) => notes.push(format!(
                            "{}: Implemented-by omitted for {subject}; {reason}",
                            patchset.id
                        )),
                    }
                }
            }
        } else if let Some(values) = state.contribution_roles.get(role) {
            for value in values {
                match crate::coordinate::coordinate_defect(value, None) {
                    None => trailers.push(format!("{role}: {value}")),
                    Some(defect) => notes.push(format!(
                        "{role} omitted for {value}; {}: withdraw it with `arc roles {} \
                         --withdraw {}`, and assert the coordinate that did the work where \
                         one is known",
                        defect.reason(),
                        state.change_id,
                        crate::context::shell_quote(&format!("{role}={value}")),
                    )),
                }
            }
        }
    }
    notes.push("review credit stays in the ledger; Reviewed-by is never projected or backfilled into commits".to_string());
    ChangeTrailers {
        schema: "arc-trailers/1",
        trailers,
        notes,
    }
}

/// The coordinate `subject` is credited with on `patchset`, or why none is.
///
/// A subject that is a fresh coordinate supplies itself. The recorder's
/// harness and model belong only to the recorder's own contribution, and only
/// when the recorder declared who wrote the work: who ran `snapshot` is not
/// proof of who wrote the diff.
fn implementation_identity(
    state: &crate::state::ChangeState,
    patchset: &crate::state::Patchset,
    subject: &str,
) -> Result<String, String> {
    use crate::coordinate::Defect;
    let declared = !patchset.contributors.is_empty()
        || patchset.on_behalf_of.is_some()
        || patchset
            .actor_source
            .is_some_and(crate::model::ActorSource::declared);
    if !declared {
        return Err(undeclared_recorder(patchset));
    }
    let defect = match crate::coordinate::coordinate_defect(subject, patchset.session.as_deref()) {
        None => return Ok(subject.trim().to_string()),
        Some(defect) => defect,
    };
    // A person's own form names a person, whose work no recorder's model
    // describes.
    if subject != patchset.actor || defect == Defect::NotAnAddress {
        let reason = match defect {
            Defect::Unreadable => "unresolved subject harness and model",
            defect => defect.reason(),
        };
        return Err(match recorded_coordinate(state, subject) {
            Some(suggestion) => format!(
                "{reason}; this change's records by that actor name {suggestion}, a \
                 suggestion to confirm and never projected"
            ),
            None => reason.to_string(),
        });
    }
    recorder_coordinate(patchset).map_err(|recorder| match defect {
        Defect::Unreadable => recorder,
        defect => format!("{}, and {recorder}", defect.reason()),
    })
}

/// Why a recorder that declared nobody is credited with nothing, what it
/// recorded, and how to declare the author.
fn undeclared_recorder(patchset: &crate::state::Patchset) -> String {
    use crate::model::ActorSource;
    let source = match patchset.actor_source {
        Some(ActorSource::Derived) => "derived from the harness session",
        Some(ActorSource::GitFallback) => "assumed from git config",
        _ => "recorded without its provenance",
    };
    let recorded = match recorder_coordinate(patchset) {
        Ok(coordinate) => format!("the recorder's model is {coordinate}"),
        Err(reason) => reason,
    };
    format!(
        "undeclared identity ({source}): who ran snapshot is not proof of who wrote the \
         diff; {recorded}; declare the author with `arc snapshot --solo` or `--contributors \
         <actor>`, and before any verdict `arc snapshot --amend {} --contributors <actor>` \
         repairs this patchset",
        patchset.id
    )
}

/// The recorder's `harness:model` when it is a fresh coordinate, or why not.
fn recorder_coordinate(patchset: &crate::state::Patchset) -> Result<String, String> {
    use crate::coordinate::{coordinate_defect, is_placeholder, model_defect};
    let session = patchset.session.as_deref();
    let harness = patchset.harness.as_deref().unwrap_or_default().trim();
    let model = patchset.model.as_deref().unwrap_or_default().trim();
    let unresolved: Vec<&str> = [("harness", harness), ("model", model)]
        .into_iter()
        .filter(|(_, value)| value.is_empty())
        .map(|(name, _)| name)
        .collect();
    if !unresolved.is_empty() {
        return Err(format!("unresolved {}", unresolved.join(", ")));
    }
    if is_placeholder(harness) {
        return Err(format!(
            "the recorded harness {harness:?} is no fallback: a placeholder is not a coordinate"
        ));
    }
    if let Some(defect) = model_defect(model, session) {
        return Err(format!(
            "the recorded model {model:?} is no fallback: {}",
            defect.reason()
        ));
    }
    let coordinate = format!("{harness}:{model}");
    match coordinate_defect(&coordinate, session) {
        None => Ok(coordinate),
        Some(defect) => Err(format!(
            "the recorded coordinate {coordinate:?} is no fallback: {}",
            defect.reason()
        )),
    }
}

/// The one fresh `harness:model` this change's own records by `actor` name,
/// when they name exactly one.
fn recorded_coordinate(state: &crate::state::ChangeState, actor: &str) -> Option<String> {
    let opening = (
        state.opened_by.as_str(),
        state.opened_harness.as_deref(),
        state.opened_model.as_deref(),
        state.opened_session.as_deref(),
    );
    let patchsets = state.patchsets.iter().map(|patchset| {
        (
            patchset.actor.as_str(),
            patchset.harness.as_deref(),
            patchset.model.as_deref(),
            patchset.session.as_deref(),
        )
    });
    let briefs = state.briefs.iter().map(|brief| {
        (
            brief.actor.as_str(),
            brief.harness.as_deref(),
            brief.model.as_deref(),
            brief.session.as_deref(),
        )
    });
    let coordinates: std::collections::BTreeSet<String> = std::iter::once(opening)
        .chain(patchsets)
        .chain(briefs)
        .filter(|(recorder, ..)| *recorder == actor)
        .filter_map(|(_, harness, model, session)| {
            let coordinate = format!("{}:{}", harness?.trim(), model?.trim());
            crate::coordinate::coordinate_defect(&coordinate, session)
                .is_none()
                .then_some(coordinate)
        })
        .collect();
    match coordinates.len() {
        1 => coordinates.into_iter().next(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implemented_by_reads_every_line_of_the_final_block() {
        let message = b"feat: x\n\nbody\n\nPlanned-by: a:b#c\nimplemented-by:  codex:gpt-6.1-sol#medium \nImplemented-by: Person <p@example.invalid>\n";
        assert_eq!(
            implemented_by(message),
            ["codex:gpt-6.1-sol#medium", "Person <p@example.invalid>"]
        );
        assert!(implemented_by(b"feat: x\n\nImplemented-by: a:b\n\nprose\n").is_empty());
        assert!(implemented_by(b"Implemented-by: a:b").is_empty());
    }

    #[test]
    fn an_effort_never_separates_a_model_from_its_implementer_line() {
        let values = vec![
            "Person <p@example.invalid>".to_string(),
            "codex:gpt-6.1-sol#medium".to_string(),
        ];
        assert_eq!(
            naming_model(&values, "gpt-6.1-sol#xhigh"),
            Some("codex:gpt-6.1-sol#medium")
        );
        assert_eq!(
            naming_model(&values, "gpt-6.1-sol"),
            Some("codex:gpt-6.1-sol#medium")
        );
        assert_eq!(naming_model(&values, "gpt-6.1-terra#medium"), None);
        assert_eq!(naming_model(&values, "#high"), None);
    }

    #[test]
    fn a_role_value_names_a_model_only_as_a_coordinate() {
        assert_eq!(
            coordinate_model("codex:gpt-6.1-sol#medium"),
            Some("gpt-6.1-sol")
        );
        assert_eq!(
            coordinate_model("claude:claude-opus-5-5"),
            Some("claude-opus-5-5")
        );
        assert_eq!(coordinate_model("Person <p@example.invalid>"), None);
        assert_eq!(coordinate_model("not a coordinate"), None);
    }

    fn edited(message: &str, drop: &[&str], append: &[&str]) -> String {
        let drop: Vec<String> = drop.iter().map(|key| key.to_string()).collect();
        let append: Vec<String> = append.iter().map(|line| line.to_string()).collect();
        String::from_utf8(edit(message.as_bytes(), &drop, &append)).unwrap()
    }

    #[test]
    fn one_key_is_dropped_and_the_rest_of_the_block_stays() {
        assert_eq!(
            edited(
                "feat: one\n\nbody\n\nFoo-Session: x\nReviewed-by: Someone <s@example.invalid>\n\
                 Assisted-by: a:b#c\n",
                &["foo-session"],
                &[],
            ),
            "feat: one\n\nbody\n\nReviewed-by: Someone <s@example.invalid>\nAssisted-by: a:b#c\n"
        );
    }

    /// A trailer's value may span lines, and the lines below it say nothing
    /// on their own — they go where the trailer they belong to goes.
    #[test]
    fn a_dropped_trailer_takes_its_continuation_lines_with_it() {
        assert_eq!(
            edited(
                "feat: one\n\nFoo: first\n  and more of it\nBar: second\n",
                &["Foo"],
                &[],
            ),
            "feat: one\n\nBar: second\n"
        );
    }

    #[test]
    fn dropping_a_key_no_trailer_carries_changes_nothing() {
        let message = "feat: one\n\nbody\n\nReviewed-by: Someone\n";
        assert_eq!(edited(message, &["absent"], &[]), message);
    }

    /// Dropping every trailer leaves no empty paragraph behind: the blank
    /// line that opened the block separated it from the body, and there is no
    /// block left to separate.
    #[test]
    fn dropping_the_whole_block_takes_its_blank_line_with_it() {
        assert_eq!(
            edited("feat: one\n\nbody\n\nFoo: x\nFoo: y\n", &["foo"], &[]),
            "feat: one\n\nbody\n"
        );
    }

    #[test]
    fn appending_to_a_message_with_no_block_opens_one() {
        assert_eq!(
            edited("feat: one\n", &[], &["Assisted-by: a:b#c"]),
            "feat: one\n\nAssisted-by: a:b#c\n"
        );
        assert_eq!(
            edited("feat: one\n\nbody\n", &[], &["Assisted-by: a:b#c"]),
            "feat: one\n\nbody\n\nAssisted-by: a:b#c\n"
        );
    }

    /// A subject spelled like a trailer is still the subject. Appending to it
    /// as though it were a block would put the line inside the summary Git
    /// shows in a log.
    #[test]
    fn a_lone_subject_is_never_a_trailer_block() {
        assert_eq!(
            edited("feat: one\n", &["feat"], &[]),
            "feat: one\n",
            "the subject is not a trailer to drop"
        );
    }

    #[test]
    fn appending_a_line_the_block_already_carries_changes_nothing() {
        let message = "feat: one\n\nReviewed-by: Someone\nAssisted-by: a:b#c\n";
        assert_eq!(edited(message, &[], &["Assisted-by: a:b#c"]), message);
    }

    /// A last paragraph holding one line of prose is prose. Appending to it
    /// would put a trailer inside a sentence, and dropping from it would take
    /// a line the writer meant as text.
    #[test]
    fn a_paragraph_that_only_partly_looks_like_a_block_is_not_one() {
        let message = "feat: one\n\nthis paragraph explains why\nFoo: x\n";
        assert_eq!(edited(message, &["foo"], &[]), message);
        assert_eq!(
            edited(message, &[], &["Assisted-by: a:b#c"]),
            "feat: one\n\nthis paragraph explains why\nFoo: x\n\nAssisted-by: a:b#c\n"
        );
    }

    /// A message is not required to be UTF-8, and an edit that decoded one
    /// would change the commit it claims to have only edited the trailers of.
    #[test]
    fn a_message_that_is_not_utf8_travels_through_byte_for_byte() {
        let message = b"f\xe9at: caf\xe9\n\nb\xf8dy\n\nFoo: \xff\xfe\nBar: keep\n";
        assert_eq!(
            edit(message, &["foo".to_string()], &[]),
            b"f\xe9at: caf\xe9\n\nb\xf8dy\n\nBar: keep\n".to_vec()
        );
        assert_eq!(
            edit(message, &[], &["Bar: keep".to_string()]),
            message.to_vec(),
            "a message the edit leaves alone is the message it was given"
        );
    }

    /// Whatever the message says outside the block says it afterwards, blank
    /// lines and trailing whitespace included.
    #[test]
    fn everything_outside_the_block_is_left_as_it_was() {
        assert_eq!(
            edited("feat: one   \n\n\nbody \n\nFoo: x\n\n\n", &["foo"], &[]),
            "feat: one   \n\n\nbody \n\n\n"
        );
        assert_eq!(
            edited("feat: one\n\nFoo: x", &[], &["Bar: y"]),
            "feat: one\n\nFoo: x\nBar: y",
            "a message that ended without a newline still does"
        );
    }

    #[test]
    fn an_empty_message_grows_only_what_is_appended() {
        assert_eq!(edited("", &["foo"], &[]), "");
        assert_eq!(edited("", &[], &["Foo: x"]), "Foo: x");
    }

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn parsed(text: &str) -> anyhow::Result<Vec<LineEdit>> {
        parse_edits(text, "edits", None)
    }

    fn edit_of(action: &str, old: &str, new: &str) -> LineEdit {
        parsed(&format!("{SHA}\t{action}\t{old}\t{new}\n"))
            .unwrap()
            .remove(0)
    }

    fn applied(message: &str, edits: &[LineEdit]) -> anyhow::Result<String> {
        let edits: Vec<&LineEdit> = edits.iter().collect();
        Ok(String::from_utf8(edit_lines(message.as_bytes(), &edits)?).unwrap())
    }

    #[test]
    fn quoted_edit_fields_represent_tabs_and_preserve_literal_escapes() {
        let old = "Planned-by:\tcodex:unknown";
        let valid = "Planned-by:\tcodex:gpt-6.1-sol#high";
        let message = format!("feat: one\n\n{old}\n{valid}\n");
        for action in ["drop", "replace"] {
            let new = if action == "drop" {
                ""
            } else {
                "Planned-by:\tR <r@example.invalid>"
            };
            let text = format!(
                "{SHA}\t{action}\t{}\t{}\n",
                serde_json::to_string(old).unwrap(),
                serde_json::to_string(new).unwrap()
            );
            let edits = parsed(&text).unwrap();
            assert_eq!(edits[0].old, old);
            assert_eq!(edits[0].new, new);
            let result = applied(&message, &edits).unwrap();
            assert!(!result.contains(old));
            assert!(result.contains(valid));
            if action == "replace" {
                assert!(result.contains(new));
            }
        }
        for line in [r"Example: literal\t", r#"Example: a "quote" and \slash"#] {
            let quoted = serde_json::to_string(line).unwrap();
            assert_eq!(
                parsed(&format!("{SHA}\tdrop\t{quoted}\n")).unwrap()[0].old,
                line
            );
            assert_eq!(
                parsed(&format!("{SHA}\tdrop\t{line}\n")).unwrap()[0].old,
                line
            );
        }
    }

    #[test]
    fn quoted_fields_refuse_invalid_json_and_multiline_trailer_text() {
        for field in [
            r#""Planned-by: bad\q""#,
            r#""A: x\nB: y""#,
            r#""A: x\rB: y""#,
        ] {
            for row in [
                format!("{SHA}\tdrop\t{field}\n"),
                format!("{SHA}\treplace\tA: x\t{field}\n"),
            ] {
                let error = parsed(&row).unwrap_err();
                assert!(error.to_string().contains("edits:1"), "{error}");
            }
        }
    }

    #[test]
    fn an_edit_names_one_line_of_one_commit_and_keeps_its_place() {
        let message = "feat: one\n\nbody\n\nPlanned-by: codex:unknown\nReviewed-by: R <r@example.invalid>\nPlanned-by: codex:gpt-6\n";
        assert_eq!(
            applied(
                message,
                &[edit_of(
                    "replace",
                    "Planned-by: codex:unknown",
                    "Planned-by: codex:gpt-6.1#high"
                )]
            )
            .unwrap(),
            "feat: one\n\nbody\n\nPlanned-by: codex:gpt-6.1#high\nReviewed-by: R <r@example.invalid>\nPlanned-by: codex:gpt-6\n"
        );
        assert_eq!(
            applied(message, &[edit_of("drop", "Planned-by: codex:unknown", "")]).unwrap(),
            "feat: one\n\nbody\n\nReviewed-by: R <r@example.invalid>\nPlanned-by: codex:gpt-6\n"
        );
    }

    #[test]
    fn dropping_the_only_trailer_leaves_no_empty_paragraph() {
        assert_eq!(
            applied(
                "feat: one\n\nPlanned-by: codex:unknown\n",
                &[edit_of("drop", "Planned-by: codex:unknown", "")]
            )
            .unwrap(),
            "feat: one\n"
        );
    }

    #[test]
    fn a_line_must_match_after_trimming_and_occur_once_as_a_whole_line() {
        let message = "feat: one\n\nPlanned-by: codex:unknown  \nPlanned-by: codex:unknown-too\n";
        assert!(applied(message, &[edit_of("drop", "Planned-by: codex:unknown", "")]).is_ok());
        let error = applied(message, &[edit_of("drop", "Planned-by: codex:unk", "")]).unwrap_err();
        assert!(error.to_string().contains("edits:1"), "{error}");
        let twice = "feat: one\n\nA: x\nA: x\n";
        let error = applied(twice, &[edit_of("drop", "A: x", "")]).unwrap_err();
        assert!(error.to_string().contains("2 times"), "{error}");
    }

    #[test]
    fn a_value_continuing_below_its_line_is_refused() {
        let error = applied(
            "feat: one\n\nFoo: first\n  and more of it\nBar: second\n",
            &[edit_of("drop", "Foo: first", "")],
        )
        .unwrap_err();
        assert!(error.to_string().contains("continues"), "{error}");
    }

    #[test]
    fn a_message_with_no_trailer_block_has_nothing_to_edit() {
        let error = applied(
            "feat: one\n\njust a body\n",
            &[edit_of("drop", "Planned-by: codex:unknown", "")],
        )
        .unwrap_err();
        assert!(error.to_string().contains("no trailer block"), "{error}");
    }

    #[test]
    fn two_edits_cannot_name_one_line() {
        let edits = [
            edit_of("drop", "A: x", ""),
            edit_of("replace", "A: x", "A: y"),
        ];
        assert!(applied("feat: one\n\nA: x\nB: z\n", &edits).is_err());
    }

    #[test]
    fn the_file_is_read_by_the_fields_and_never_by_position_alone() {
        assert!(parsed("").unwrap().is_empty());
        assert!(parsed("# arc-trailer-edits/1\n\n  \n# a note\n")
            .unwrap()
            .is_empty());
        let upper = SHA.to_ascii_uppercase();
        let edits = parsed(&format!("{upper}\tdrop\tA: x\r\n")).unwrap();
        assert_eq!(edits[0].commit, SHA, "a sha is kept in lower case");
        assert_eq!(edits[0].origin, "edits:1");
        let wide = "a".repeat(64);
        assert!(parsed(&format!("{wide}\tdrop\tA: x\t\n")).is_ok());
        for bad in [
            format!("{SHA}\tdrop\n"),
            format!("{SHA}\tdrop\tA: x\t\textra\n"),
            format!("{}\tdrop\tA: x\t\n", &SHA[..39]),
            format!("{SHA}\tkeep\tA: x\t\n"),
            format!("{SHA}\tdrop\tnot a trailer\t\n"),
            format!("{SHA}\tdrop\tA: x\tB: y\n"),
            format!("{SHA}\treplace\tA: x\t\n"),
            format!("{SHA}\treplace\tA: x\tprose\n"),
            "# arc-trailer-edits/2\n".to_string(),
        ] {
            assert!(parsed(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_replacement_on_a_role_key_is_a_fresh_coordinate() {
        for key in [
            "Planned-by",
            "implemented-by",
            "Reviewed-by",
            "Orchestrated-by",
        ] {
            for bad in ["codex:unknown", "codex", "Name <name>", "?:model"] {
                let line = format!("{SHA}\treplace\t{key}: codex:x\t{key}: {bad}\n");
                assert!(parsed(&line).is_err(), "{line:?}");
            }
            let line = format!("{SHA}\treplace\t{key}: codex:x\t{key}: codex:gpt-6.1#high\n");
            assert!(parsed(&line).is_ok(), "{line:?}");
        }
        // Other keys are not judged, and an Assisted-by value is judged
        // without the role in brackets.
        assert!(parsed(&format!("{SHA}\treplace\tA: x\tSigned-off-by: nobody\n")).is_ok());
        assert!(parsed(&format!(
            "{SHA}\treplace\tAssisted-by: a:b\tAssisted-by: codex:gpt-6 (review)\n"
        ))
        .is_ok());
        assert!(parsed(&format!(
            "{SHA}\treplace\tAssisted-by: a:b\tAssisted-by: codex:unknown (review)\n"
        ))
        .is_err());
        assert!(parsed(&format!(
            "{SHA}\treplace\tAssisted-by: a:b\tAssisted-by: codex:unknown\n"
        ))
        .is_err());
    }
}
