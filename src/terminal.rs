//! What a view for a person may draw on standard output: colour, and how
//! wide a line is. A view asks once and draws the same text either way;
//! colour only paints it.

use std::io::IsTerminal;

pub const BOLD: &str = "1";
pub const DIM: &str = "2";
pub const RED: &str = "31";
pub const GREEN: &str = "32";
pub const YELLOW: &str = "33";
pub const CYAN: &str = "36";

#[derive(Clone, Copy)]
pub struct Look {
    /// Whether text is painted. A pipe, a file and a `dumb` terminal are
    /// never painted, and neither is anything while `NO_COLOR` is set to
    /// more than the empty string.
    pub color: bool,
    /// The terminal's columns; absent when standard output is not one, and
    /// text is then never wrapped.
    pub width: Option<usize>,
}

impl Look {
    pub fn of_stdout() -> Self {
        let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
        let dumb = std::env::var_os("TERM").is_some_and(|term| term == "dumb");
        Self {
            color: std::io::stdout().is_terminal() && !set("NO_COLOR") && !dumb,
            width: columns(),
        }
    }

    /// `text` in one SGR style, or as it is when nothing is painted.
    pub fn paint(&self, style: &str, text: &str) -> String {
        if self.color && !text.is_empty() {
            format!("\x1b[{style}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    /// `text` filled into lines that fit beside an indent of `indent`
    /// columns. One line when the width is unknown; a word wider than the
    /// room sits alone.
    pub fn wrap(&self, text: &str, indent: usize) -> Vec<String> {
        match self.width {
            Some(width) => fill(text, width.saturating_sub(indent).max(20)),
            None => vec![text.to_string()],
        }
    }
}

/// `text`'s words filled into lines of at most `room` columns; a word wider
/// than that sits alone.
pub fn fill(text: &str, room: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match lines.last_mut() {
            Some(line) if line.chars().count() + 1 + word.chars().count() <= room => {
                line.push(' ');
                line.push_str(word);
            }
            _ => lines.push(word.to_string()),
        }
    }
    lines
}

#[cfg(unix)]
fn columns() -> Option<usize> {
    let mut size = std::mem::MaybeUninit::<libc::winsize>::zeroed();
    // SAFETY: the request writes one `winsize` into the storage it is given,
    // and fails without writing when standard output is not a terminal.
    let size = unsafe {
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, size.as_mut_ptr()) != 0 {
            return None;
        }
        size.assume_init()
    };
    (size.ws_col > 0).then_some(size.ws_col as usize)
}

#[cfg(not(unix))]
fn columns() -> Option<usize> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unpainted text is the text itself. Rejects escape bytes reaching a
    /// pipe or a file.
    #[test]
    fn nothing_is_painted_without_colour() {
        let look = Look {
            color: false,
            width: None,
        };
        assert_eq!(look.paint(GREEN, "done"), "done");
        assert_eq!(
            Look {
                color: true,
                width: None
            }
            .paint(GREEN, "done"),
            "\x1b[32mdone\x1b[0m"
        );
    }

    /// Wrapped lines fit the room beside the indent and keep every word.
    /// Rejects a line that runs past the terminal's edge and a width guessed
    /// for output that is not a terminal.
    #[test]
    fn text_wraps_only_to_a_known_width() {
        let text = "one two three four five six seven eight nine ten";
        let unknown = Look {
            color: false,
            width: None,
        };
        assert_eq!(unknown.wrap(text, 8), [text]);
        let narrow = Look {
            color: false,
            width: Some(30),
        };
        let lines = narrow.wrap(text, 8);
        assert!(lines.len() > 1, "{lines:?}");
        assert!(lines.iter().all(|line| line.chars().count() <= 22));
        assert_eq!(lines.join(" "), text);
    }
}
