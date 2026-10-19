//! The compact text form of a cost report: one row per session, the union
//! total, what of each session is outside it and why, a count of the
//! qualifications `--detail` explains, and a subtotal per group when the
//! view groups rows. Every value comes from the `table` the `arc-cost/1`
//! document carries, so the text and the JSON never disagree.

use serde_json::Value;
use std::fmt::Write as _;

/// The money columns a table shows: the list-price estimate then the
/// harness-written figure when a price table is loaded, else the harness
/// figure alone.
fn money_columns(priced: bool) -> &'static [(&'static str, &'static str)] {
    if priced {
        &[("list", "list"), ("usd", "harness")]
    } else {
        &[("usd", "money")]
    }
}

fn headers(priced: bool) -> Vec<String> {
    let mut headers: Vec<String> = ["agent", "roles", "session", "runs", "spans", "tokens"]
        .iter()
        .map(|header| header.to_string())
        .collect();
    headers.extend(
        money_columns(priced)
            .iter()
            .map(|(_, header)| header.to_string()),
    );
    headers.push("status".to_string());
    headers
}

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

fn money_cells(figure: &Value, priced: bool) -> impl Iterator<Item = String> + '_ {
    money_columns(priced)
        .iter()
        .map(move |(field, _)| shown(&figure[*field], money_text))
}

fn figure_row(label: String, figure: &Value, priced: bool) -> Vec<String> {
    let mut row = vec![
        label,
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        shown(&figure["tokens"], token_text),
    ];
    row.extend(money_cells(figure, priced));
    row.push(status(&figure["status"]));
    row
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

fn session_row(row: &Value, priced: bool) -> Vec<String> {
    let roles: Vec<&str> = row["roles"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(role)
        .collect();
    let mut cells = vec![
        status(&row["agent"]),
        roles.join(","),
        status(&row["session"]),
        row["runs"].to_string(),
        row["spans"].to_string(),
        shown(&row["tokens"], token_text),
    ];
    cells.extend(money_cells(row, priced));
    cells.push(status(&row["status"]));
    cells
}

/// Each accounting partition of a combined figure, as its own row.
fn partition_rows(label: &str, figure: &Value, priced: bool) -> Vec<Vec<String>> {
    let width = headers(priced).len();
    figure["partitions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|partition| {
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
            let mut row = vec![String::new(); width];
            row[0] = format!("  {label} {domain}");
            let cell = &partition["cell"];
            match money_columns(priced)
                .iter()
                .position(|(field, _)| partition["column"] == *field)
            {
                Some(index) => row[6 + index] = shown(cell, money_text),
                None if partition["column"] == "tokens" => row[5] = shown(cell, token_text),
                None => return None,
            }
            row[width - 1] = status(&cell["status"]);
            Some(row)
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
    let columns = rows.first().map_or(0, Vec::len);
    let width = |column: usize| {
        rows.iter()
            .map(|row| row[column].chars().count())
            .max()
            .unwrap_or(0)
    };
    let widths: Vec<usize> = (0..columns).map(width).collect();
    for row in rows {
        let mut line = String::new();
        for (column, value) in row.iter().enumerate() {
            let pad = widths[column].saturating_sub(value.chars().count());
            // Counts and amounts align right; names and words align left.
            if (3..columns - 1).contains(&column) {
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
    let priced = table["prices"]["status"] == "loaded";
    let mut out = String::new();
    let _ = writeln!(out, "{}", heading(report));
    let mut rows = vec![headers(priced)];
    let sessions: Vec<&Value> = table["rows"].as_array().into_iter().flatten().collect();
    let groups: Vec<&Value> = table["groups"].as_array().into_iter().flatten().collect();
    if groups.is_empty() {
        rows.extend(sessions.iter().map(|row| session_row(row, priced)));
    } else {
        for group in &groups {
            let members: Vec<&Value> = group["sessions"].as_array().into_iter().flatten().collect();
            rows.extend(
                sessions
                    .iter()
                    .filter(|row| members.contains(&&row["key"]))
                    .map(|row| session_row(row, priced)),
            );
            let label = format!("subtotal {}", group_label(group));
            rows.push(figure_row(label.clone(), group, priced));
            rows.extend(partition_rows("subtotal", group, priced));
        }
        rows.extend(
            sessions
                .iter()
                .filter(|row| row["counted"] != true && row["share"] != true)
                .map(|row| session_row(row, priced)),
        );
    }
    rows.push(figure_row("total".to_string(), &table["total"], priced));
    rows.extend(partition_rows("total", &table["total"], priced));
    if table["estimated"].is_object() {
        rows.push(figure_row(
            format!(
                "total with estimates (working stretches, idle {})",
                table["estimated"]["estimate"]["idle"]
                    .as_str()
                    .unwrap_or_default()
            ),
            &table["estimated"],
            priced,
        ));
        rows.extend(partition_rows("estimated", &table["estimated"], priced));
    }
    if table["allocated"].is_object() {
        rows.push(figure_row(
            format!(
                "total allocated {}",
                table["allocated"]["allocation"]
                    .as_str()
                    .unwrap_or_default()
            ),
            &table["allocated"],
            priced,
        ));
        rows.extend(partition_rows("allocated", &table["allocated"], priced));
    }
    let unattributed: Vec<&Value> = table["unattributed"]["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    if !unattributed.is_empty() {
        rows.push(figure_row(
            format!(
                "unattributed shared ({} session{})",
                unattributed.len(),
                if unattributed.len() == 1 { "" } else { "s" }
            ),
            &table["unattributed"],
            priced,
        ));
        rows.extend(partition_rows(
            "unattributed",
            &table["unattributed"],
            priced,
        ));
        for session in &unattributed {
            rows.push(figure_row(
                format!("  {}", status(&session["session"])),
                session,
                priced,
            ));
        }
    }
    render(&mut out, &rows);
    for session in &unattributed {
        let reasons: Vec<&str> = session["reasons"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let _ = writeln!(
            out,
            "unattributed {}: {}",
            status(&session["session"]),
            reasons.join(", ")
        );
    }
    for family in table["families"].as_array().into_iter().flatten() {
        let _ = writeln!(out, "{}", family_line(family));
        if let Some(line) = residual_line(family) {
            let _ = writeln!(out, "{line}");
        }
    }
    if let Some(footprint) = report["footprint"]["session"].as_str() {
        let _ = writeln!(
            out,
            "footprint {footprint}: whole figure {}",
            report["footprint"]["figure"]
                .as_str()
                .unwrap_or("not measured here")
        );
    }
    let _ = writeln!(out, "{}", prices_line(&table["prices"]));
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

/// A parent's native children: those bound to rows, and the consumption no
/// binding assigns, which stays outside the total.
fn family_line(family: &Value) -> String {
    let bound: Vec<&str> = family["bound"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut parts = vec![if bound.is_empty() {
        "none bound".to_string()
    } else {
        format!("{} bound ({})", bound.len(), bound.join(", "))
    }];
    let elsewhere = family["elsewhere"].as_u64().unwrap_or(0);
    if elsewhere > 0 {
        parts.push(format!("{elsewhere} bound to unselected work"));
    }
    let unassigned = &family["unassigned"];
    let count = unassigned["children"].as_u64().unwrap_or(0);
    if count > 0 {
        let figure = shown(&unassigned["tokens"], token_text);
        let unmeasured = unassigned["unmeasured"].as_u64().unwrap_or(0);
        let mut part = format!("{count} unassigned {figure} outside the total");
        if unmeasured > 0 {
            part.push_str(&format!(" ({unmeasured} not measured)"));
        }
        parts.push(part);
    }
    let missing = family["missing"].as_u64().unwrap_or(0);
    if missing > 0 {
        parts.push(format!(
            "{missing} recording{} missing",
            if missing == 1 { "" } else { "s" }
        ));
    }
    format!(
        "children {}: {}",
        status(&family["session"]),
        parts.join("; ")
    )
}

/// The recorded total less the family's identified requests, per counter
/// that does not reconcile: the parent's, outside every total.
fn residual_line(family: &Value) -> Option<String> {
    let reconciliation = family["reconciliation"].as_str()?;
    if reconciliation == "unavailable" {
        return Some(format!(
            "family {}: reconciliation unavailable",
            status(&family["session"])
        ));
    }
    let counters: Vec<String> = family["residual"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|counter| {
            let name = status(&counter["counter"]);
            let state = status(&counter["status"]);
            match counter["difference"].as_i64() {
                Some(difference) => {
                    let sign = if difference < 0 { "-" } else { "+" };
                    format!("{name} {sign}{} {state}", tokens(difference.unsigned_abs()))
                }
                None => format!("{name} {state}"),
            }
        })
        .collect();
    let residual = if counters.is_empty() {
        "residual reconciled".to_string()
    } else {
        format!("residual {}, outside the total", counters.join(", "))
    };
    Some(format!(
        "family {}: requests {reconciliation}; {residual}",
        status(&family["session"])
    ))
}

/// Where the money columns come from, and how to install a price table when
/// none is loaded.
fn prices_line(prices: &Value) -> String {
    let schema = prices["schema"].as_str().unwrap_or("tapes-price-table/1");
    match prices["status"].as_str() {
        Some("loaded") => format!(
            "money: list price by a {schema} table ({}) beside the harness-written estimate",
            match prices["origin"].as_str() {
                Some("named") => "--prices",
                Some("environment") => "TAPES_PRICES",
                _ => "the default location",
            }
        ),
        Some("invalid") => format!(
            "money: harness-written estimates; the {schema} table at the default location is invalid, so no list price is shown"
        ),
        _ => format!(
            "money: harness-written estimates; no price table is loaded\n  install a {schema} file at ~/.config/tapes/prices.toml, or name one with --prices or TAPES_PRICES"
        ),
    }
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
