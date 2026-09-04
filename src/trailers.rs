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

/// Edit a message's trailers: drop every trailer whose key is one of `drop`,
/// then add each line of `append` the block does not already carry verbatim.
///
/// The answer is the message the commit is to hold. It equals the original
/// exactly when the edit had nothing to do, which is how a caller tells a
/// commit it must recreate from one it must leave alone.
pub fn edit(message: &[u8], drop: &[String], append: &[String]) -> Vec<u8> {
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

    let mut kept: Vec<Vec<u8>> = Vec::new();
    if let Some(block) = &block {
        // A continuation line says nothing on its own, so it goes wherever
        // the trailer above it went.
        let mut dropping = false;
        for line in &lines[block.clone()] {
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

    let (before, after) = match &block {
        Some(block) => (&lines[..block.start], &lines[block.end..]),
        // A message with no block grows one after everything it says, with
        // the blank line that separates a paragraph.
        None => (&lines[..], &lines[lines.len()..]),
    };
    if block.is_none() && kept.is_empty() {
        return message.to_vec();
    }

    let mut out: Vec<Vec<u8>> = before.iter().map(|line| line.to_vec()).collect();
    if kept.is_empty() {
        // Dropping every trailer leaves an empty paragraph, and the blank
        // line that opened it separates nothing.
        while out.last().is_some_and(|line| is_blank(line)) {
            out.pop();
        }
    } else {
        if block.is_none() && out.last().is_some_and(|line| !is_blank(line)) {
            out.push(Vec::new());
        }
        out.extend(kept);
    }
    out.extend(after.iter().map(|line| line.to_vec()));

    let mut edited = out.join(&b'\n');
    if ends_with_newline && !edited.is_empty() {
        edited.push(b'\n');
    }
    edited
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
