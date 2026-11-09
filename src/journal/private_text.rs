//! Private text a published page must not carry: paths on the machine that
//! wrote the journal, and network addresses only its owner can reach.
//!
//! The scan is deliberately narrow. A path under someone's home, or under a
//! directory arc itself knows is local, names a person or a machine; an
//! address in a private, shared or link-local range, or a host under a
//! private suffix, maps a network. A system path such as `/usr/lib` or a
//! loopback address says nothing about anyone and is left alone.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;

/// One stretch of private text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Finding {
    /// 1-based line within the scanned text.
    pub(super) line: usize,
    pub(super) kind: Kind,
    pub(super) text: String,
    /// Byte range within the line.
    start: usize,
    end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    LocalPath,
    PrivateAddress,
    PrivateHost,
}

impl Kind {
    pub(super) fn label(self) -> &'static str {
        match self {
            Kind::LocalPath => "local path",
            Kind::PrivateAddress => "private address",
            Kind::PrivateHost => "private host",
        }
    }

    fn placeholder(self) -> &'static str {
        match self {
            Kind::LocalPath => "[local path]",
            Kind::PrivateAddress => "[private address]",
            Kind::PrivateHost => "[private host]",
        }
    }
}

/// Host suffixes that only resolve inside one person's network: multicast
/// DNS, home routers, and a tailnet's names.
const PRIVATE_SUFFIXES: [&str; 4] = [".local", ".lan", ".home.arpa", ".ts.net"];

/// Path prefixes that name a person's home on any machine.
const HOME_PREFIXES: [&str; 2] = ["/home/", "/Users/"];

pub(super) struct Scanner {
    /// Absolute directories known to be local to this machine, longest first.
    roots: Vec<String>,
}

impl Scanner {
    /// A scanner that also treats each of `roots` as local. Roots shorter
    /// than two components are ignored: `/` or `/srv` would flag every path.
    pub(super) fn new(roots: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut roots: Vec<String> = roots
            .into_iter()
            .flat_map(|root| {
                let canonical = std::fs::canonicalize(&root).ok();
                [Some(root), canonical]
            })
            .flatten()
            .filter_map(|root| {
                let text = root.to_str()?.trim_end_matches('/').to_string();
                (root.is_absolute() && text.matches('/').count() >= 2).then_some(text)
            })
            .collect();
        roots.sort_by_key(|root| std::cmp::Reverse(root.len()));
        roots.dedup();
        Self { roots }
    }

    /// Every stretch of private text, in reading order.
    pub(super) fn findings(&self, text: &str) -> Vec<Finding> {
        let mut findings = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let mut found: Vec<Finding> = Vec::new();
            for (start, token) in tokens(line) {
                for (offset, end, kind) in self.token_findings(token) {
                    found.push(Finding {
                        line: index + 1,
                        kind,
                        text: token[offset..end].to_string(),
                        start: start + offset,
                        end: start + end,
                    });
                }
            }
            found.sort_by_key(|finding| finding.start);
            let mut last_end = 0;
            for finding in found {
                if finding.start >= last_end {
                    last_end = finding.end;
                    findings.push(finding);
                }
            }
        }
        findings
    }

    /// `text` with each finding replaced by a placeholder naming its kind.
    pub(super) fn strip(&self, text: &str) -> String {
        let findings = self.findings(text);
        if findings.is_empty() {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        for (index, line) in text.split_inclusive('\n').enumerate() {
            let mut cursor = 0;
            for finding in findings.iter().filter(|finding| finding.line == index + 1) {
                out.push_str(&line[cursor..finding.start]);
                out.push_str(finding.kind.placeholder());
                cursor = finding.end;
            }
            out.push_str(&line[cursor..]);
        }
        out
    }

    /// The private stretches inside one token, as byte ranges of it.
    fn token_findings(&self, token: &str) -> Vec<(usize, usize, Kind)> {
        let mut found = Vec::new();
        if let Some(start) = self.local_path_start(token) {
            found.push((start, token.len(), Kind::LocalPath));
            return found;
        }
        for (start, run) in runs(token, |c| c.is_ascii_digit() || c == '.') {
            if run.split('.').count() == 4 {
                if let Ok(address) = run.parse::<Ipv4Addr>() {
                    if private_v4(address) {
                        found.push((start, start + run.len(), Kind::PrivateAddress));
                    }
                }
            }
        }
        for (start, run) in runs(token, |c| c.is_ascii_hexdigit() || c == ':') {
            if run.matches(':').count() >= 2 {
                if let Ok(address) = run.parse::<Ipv6Addr>() {
                    if private_v6(address) {
                        found.push((start, start + run.len(), Kind::PrivateAddress));
                    }
                }
            }
        }
        // `_` never appears in a host name, but it continues an identifier,
        // so `self.local_state` is not read as the host `self.local`.
        for (start, run) in runs(token, |c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')
        }) {
            let run = run.trim_end_matches('.');
            let lower = run.to_ascii_lowercase();
            let private = run.starts_with(|c: char| c.is_ascii_alphanumeric())
                && PRIVATE_SUFFIXES
                    .iter()
                    .any(|suffix| lower.ends_with(suffix) && lower.len() > suffix.len());
            if private {
                found.push((start, start + run.len(), Kind::PrivateHost));
            }
        }
        found
    }

    /// Where a local path begins inside `token`: a known root or a home
    /// prefix, at the start or after a separator such as `=` or `file://`.
    fn local_path_start(&self, token: &str) -> Option<usize> {
        let boundary = |at: usize| {
            at == 0
                || !token[..at]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
        };
        let rooted = self.roots.iter().find_map(|root| {
            token.match_indices(root.as_str()).find_map(|(at, _)| {
                let after = token[at + root.len()..].chars().next();
                (boundary(at) && after.is_none_or(|c| c == '/')).then_some(at)
            })
        });
        let home = HOME_PREFIXES.iter().find_map(|prefix| {
            token.match_indices(prefix).find_map(|(at, _)| {
                let user = token[at + prefix.len()..].chars().next();
                (boundary(at) && user.is_some_and(|c| c.is_ascii_alphanumeric())).then_some(at)
            })
        });
        let windows = token
            .to_ascii_lowercase()
            .find(":\\users\\")
            .and_then(|at| at.checked_sub(1));
        [rooted, home, windows].into_iter().flatten().min()
    }
}

/// Maximal stretches of a line that can hold a path or an address, with
/// their byte offsets. Quotes, brackets, backticks and whitespace end one;
/// trailing sentence punctuation is not part of it.
fn tokens(line: &str) -> Vec<(usize, &str)> {
    let separator = |c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '`' | '"'
                    | '\''
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '<'
                    | '>'
                    | '{'
                    | '}'
                    | '|'
                    | ','
                    | ';'
                    | '*'
            )
    };
    runs(line, |c| !separator(c))
        .into_iter()
        .map(|(start, token)| (start, token.trim_end_matches(['.', ':', '!', '?'])))
        .filter(|(_, token)| !token.is_empty())
        .collect()
}

fn runs(text: &str, member: impl Fn(char) -> bool) -> Vec<(usize, &str)> {
    let mut runs = Vec::new();
    let mut start = None;
    for (at, c) in text.char_indices() {
        match (member(c), start) {
            (true, None) => start = Some(at),
            (false, Some(from)) => {
                runs.push((from, &text[from..at]));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(from) = start {
        runs.push((from, &text[from..]));
    }
    runs
}

/// RFC 1918 private ranges, the shared address space carriers and overlay
/// networks use (100.64.0.0/10), and link-local addresses.
fn private_v4(address: Ipv4Addr) -> bool {
    let [first, second, ..] = address.octets();
    address.is_private() || address.is_link_local() || (first == 100 && (64..128).contains(&second))
}

/// Unique local (fc00::/7) and link-local (fe80::/10) IPv6 addresses.
fn private_v6(address: Ipv6Addr) -> bool {
    let first = address.segments()[0];
    (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(scanner: &Scanner, text: &str) -> Vec<(Kind, String)> {
        scanner
            .findings(text)
            .into_iter()
            .map(|finding| (finding.kind, finding.text))
            .collect()
    }

    #[test]
    fn home_paths_and_known_roots_are_local() {
        let scanner = Scanner::new([PathBuf::from("/srv/work/arc")]);
        assert_eq!(
            kinds(&scanner, "see /home/alice/notes.md and /srv/work/arc/src."),
            vec![
                (Kind::LocalPath, "/home/alice/notes.md".to_string()),
                (Kind::LocalPath, "/srv/work/arc/src".to_string()),
            ]
        );
        assert_eq!(
            kinds(&scanner, "HOME=/Users/bob file:///home/carol/x"),
            vec![
                (Kind::LocalPath, "/Users/bob".to_string()),
                (Kind::LocalPath, "/home/carol/x".to_string()),
            ]
        );
        assert_eq!(
            kinds(&scanner, r"C:\Users\dana\x"),
            vec![(Kind::LocalPath, r"C:\Users\dana\x".to_string())]
        );
    }

    #[test]
    fn generic_paths_and_loopback_are_left_alone() {
        let scanner = Scanner::new([PathBuf::from("/srv/work/arc"), PathBuf::from("/")]);
        for text in [
            "/usr/lib/libwayland.so and ~/.config/tray",
            "/srv/work/archive is a sibling, not under the root",
            "binds 127.0.0.1:8080 or ::1 on localhost",
            "version 1.2.3.4 of 8.8.8.8",
            "~/.local/share and config.toml",
            "the /home/ directory itself",
            "self.local_state and host.docker.internal",
        ] {
            assert_eq!(kinds(&scanner, text), vec![], "{text}");
        }
    }

    #[test]
    fn private_networks_and_hosts_are_flagged() {
        let scanner = Scanner::new(Vec::<PathBuf>::new());
        assert_eq!(
            kinds(
                &scanner,
                "http://192.168.1.20:8080/x, 10.0.0.5, 100.101.102.103 and 172.16.4.4."
            ),
            vec![
                (Kind::PrivateAddress, "192.168.1.20".to_string()),
                (Kind::PrivateAddress, "10.0.0.5".to_string()),
                (Kind::PrivateAddress, "100.101.102.103".to_string()),
                (Kind::PrivateAddress, "172.16.4.4".to_string()),
            ]
        );
        assert_eq!(
            kinds(
                &scanner,
                "ssh gak.local or nas.tail1234.ts.net via fd12:3456::1"
            ),
            vec![
                (Kind::PrivateHost, "gak.local".to_string()),
                (Kind::PrivateHost, "nas.tail1234.ts.net".to_string()),
                (Kind::PrivateAddress, "fd12:3456::1".to_string()),
            ]
        );
    }

    #[test]
    fn strip_replaces_each_finding_and_keeps_the_rest() {
        let scanner = Scanner::new(Vec::<PathBuf>::new());
        assert_eq!(
            scanner.strip("read /home/alice/a.md\nthen ping 192.168.0.2.\nok\n"),
            "read [local path]\nthen ping [private address].\nok\n"
        );
    }
}
