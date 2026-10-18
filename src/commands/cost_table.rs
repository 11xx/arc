//! The compact text form of a cost report: one row per session, the union
//! total, the sessions outside it, a count of the qualifications `--detail`
//! explains, and a subtotal per group when the view groups rows. Every value
//! comes from the `table` the `arc-cost/1` document carries, so the text and
//! the JSON never disagree.

use serde_json::Value;
use std::fmt::Write as _;

const HEADERS: [&str; 8] = [
    "agent", "roles", "session", "runs", "spans", "tokens", "money", "status",
];

/// A token count at a glance: whole below a thousand, then thousands,
/// millions and billions with three significant figures.
fn tokens(count: u64) -> String {
    let scaled = |unit: f64, suffix: &str| {
        let value = count as f64 / unit;
        let decimals = if value < 10.0 {
            2
        } else if value < 100.0 {
            1
        } else {
            0
        };
        format!("{value:.decimals$}{suffix}")
    };
    match count {
        0..1_000 => count.to_string(),
        1_000..1_000_000 => scaled(1e3, "k"),
        1_000_000..1_000_000_000 => scaled(1e6, "M"),
        _ => scaled(1e9, "B"),
    }
}

fn money(amount: f64) -> String {
    format!("${amount:.4}")
}

/// What a cell shows: its amount, its known subtotal, or the least an
/// overlapping figure is known to be; `--detail` shows both ends of that
/// bound. A cell with none of them is blank.
fn shown(cell: &Value, render: fn(&Value) -> Option<String>) -> String {
    if let Some(text) = render(&cell["amount"]).or_else(|| render(&cell["known_subtotal"])) {
        return text;
    }
    let bound = if cell["bound"].is_object() {
        &cell["bound"]
    } else {
        &cell["known_bound"]
    };
    render(&bound["at_least"]).map_or_else(String::new, |low| format!("≥{low}"))
}

/// A cell's text in the token or the money column.
pub(super) fn cell_text(cell: &Value, usd: bool) -> String {
    shown(cell, if usd { money_text } else { token_text })
}

fn token_text(value: &Value) -> Option<String> {
    value.as_u64().map(tokens)
}

fn money_text(value: &Value) -> Option<String> {
    value.as_f64().map(money)
}

fn status(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

fn figure_row(label: String, figure: &Value) -> Vec<String> {
    vec![
        label,
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        shown(&figure["tokens"], token_text),
        shown(&figure["usd"], money_text),
        status(&figure["status"]),
    ]
}

/// A role as the table abbreviates it, so a row fits its line.
fn role(name: &str) -> &str {
    match name {
        "implementer" => "impl",
        "integrator" => "integ",
        "orchestrator" => "orch",
        "reviewer" => "review",
        other => other,
    }
}

fn session_row(row: &Value) -> Vec<String> {
    let roles: Vec<&str> = row["roles"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(role)
        .collect();
    vec![
        status(&row["agent"]),
        roles.join(","),
        status(&row["session"]),
        row["runs"].to_string(),
        row["spans"].to_string(),
        shown(&row["tokens"], token_text),
        shown(&row["usd"], money_text),
        status(&row["status"]),
    ]
}

/// Each accounting partition of a combined figure, as its own row.
fn partition_rows(label: &str, figure: &Value) -> Vec<Vec<String>> {
    figure["partitions"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|partition| {
            let domain = [&partition["harness"], &partition["accounting_basis"]]
                .into_iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("/");
            let domain = if domain.is_empty() {
                "basis unknown".to_string()
            } else {
                domain
            };
            let mut row = vec![String::new(); HEADERS.len()];
            row[0] = format!("  {label} {domain}");
            let cell = &partition["cell"];
            if partition["column"] == "usd" {
                row[6] = shown(cell, money_text);
            } else {
                row[5] = shown(cell, token_text);
            }
            row[7] = status(&cell["status"]);
            row
        })
        .collect()
}

fn group_label(group: &Value) -> String {
    group["by"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(grouping, value)| format!("{grouping} {}", value.as_str().unwrap_or_default()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn render(out: &mut String, rows: &[Vec<String>]) {
    let width = |column: usize| {
        rows.iter()
            .map(|row| row[column].chars().count())
            .max()
            .unwrap_or(0)
    };
    let widths: Vec<usize> = (0..HEADERS.len()).map(width).collect();
    for row in rows {
        let mut line = String::new();
        for (column, value) in row.iter().enumerate() {
            let pad = widths[column].saturating_sub(value.chars().count());
            // Counts and amounts align right; names and words align left.
            if (3..=6).contains(&column) {
                line.push_str(&" ".repeat(pad));
                line.push_str(value);
            } else {
                line.push_str(value);
                line.push_str(&" ".repeat(pad));
            }
            if column + 1 < row.len() {
                line.push_str("  ");
            }
        }
        let _ = writeln!(out, "{}", line.trim_end());
    }
}

fn heading(report: &Value) -> String {
    let target = &report["target"];
    match target["kind"].as_str() {
        Some("journal") => format!("cost: journal {}", status(&target["file"])),
        Some("range") => {
            let changes = target["changes"].as_u64().unwrap_or(0);
            let journals = target["journals"].as_u64().unwrap_or(0);
            let plural = |count: u64, one: &str, many: &str| {
                format!("{count} {}", if count == 1 { one } else { many })
            };
            format!(
                "cost: {}, {}",
                plural(changes, "change", "changes"),
                plural(journals, "journal item", "journal items")
            )
        }
        _ => format!("cost: change {}", status(&target["id"])),
    }
}

/// The compact table of a cost report.
pub(super) fn table_text(report: &Value) -> String {
    let table = &report["table"];
    let mut out = String::new();
    let _ = writeln!(out, "{}", heading(report));
    let mut rows = vec![HEADERS.iter().map(|header| header.to_string()).collect()];
    let sessions: Vec<&Value> = table["rows"].as_array().into_iter().flatten().collect();
    let groups: Vec<&Value> = table["groups"].as_array().into_iter().flatten().collect();
    if groups.is_empty() {
        rows.extend(sessions.iter().map(|row| session_row(row)));
    } else {
        for group in &groups {
            let members: Vec<&Value> = group["sessions"].as_array().into_iter().flatten().collect();
            rows.extend(
                sessions
                    .iter()
                    .filter(|row| members.contains(&&row["key"]))
                    .map(|row| session_row(row)),
            );
            let label = format!("subtotal {}", group_label(group));
            rows.push(figure_row(label.clone(), group));
            rows.extend(partition_rows("subtotal", group));
        }
        rows.extend(
            sessions
                .iter()
                .filter(|row| row["counted"] != true)
                .map(|row| session_row(row)),
        );
    }
    rows.push(figure_row("total".to_string(), &table["total"]));
    rows.extend(partition_rows("total", &table["total"]));
    let shared = table["shared"]["sessions"].as_array().map_or(0, Vec::len);
    if shared > 0 {
        rows.push(figure_row(
            format!("shared ({shared} outside the total)"),
            &table["shared"],
        ));
        rows.extend(partition_rows("shared", &table["shared"]));
    }
    render(&mut out, &rows);
    if let Some(footprint) = report["footprint"]["session"].as_str() {
        let _ = writeln!(
            out,
            "footprint {footprint}: whole figure {}",
            report["footprint"]["figure"]
                .as_str()
                .unwrap_or("not measured here")
        );
    }
    let qualifications = table["coverage"]["qualifications"].as_u64().unwrap_or(0);
    let _ = writeln!(
        out,
        "coverage: {}; arc cost show --detail names each",
        if qualifications == 0 {
            "no qualifications".to_string()
        } else {
            let kinds: Vec<String> = table["coverage"]["by_kind"]
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(_, count)| count.as_u64().unwrap_or(0) > 0)
                .map(|(kind, count)| format!("{kind} {count}"))
                .collect();
            format!(
                "{qualifications} qualification{} ({})",
                if qualifications == 1 { "" } else { "s" },
                kinds.join(", ")
            )
        }
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_counts_read_at_a_glance() {
        assert_eq!(tokens(999), "999");
        assert_eq!(tokens(1_000), "1.00k");
        assert_eq!(tokens(312_400), "312k");
        assert_eq!(tokens(4_020_000), "4.02M");
        assert_eq!(tokens(38_959_431), "39.0M");
        assert_eq!(tokens(1_200_000_000), "1.20B");
    }

    #[test]
    fn money_has_fixed_decimals() {
        assert_eq!(money(0.05), "$0.0500");
        assert_eq!(money(2.1808566), "$2.1809");
    }
}
