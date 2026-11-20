//! CSV / XML output (§20.5), Node `toCSV` / `toXML` parity on JSON rows.

use serde_json::Value;
use std::fmt::Write;

/// JS `String(v)` for scalars, `JSON.stringify` for objects; null → `""`. Text that a
/// spreadsheet would run as a formula (`= + - @`, tab or CR first) gets a leading `'`.
fn cell(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) if s.starts_with(['=', '+', '-', '@', '\t', '\r']) => {
            format!("'{s}")
        }
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// Header = keys of the first row; lines joined by `\n`, no trailing newline.
#[must_use]
pub fn format_csv(rows: &[Value]) -> String {
    let Some(Value::Object(first)) = rows.first() else {
        return String::new();
    };
    let headers: Vec<&String> = first.keys().collect();
    let mut lines = vec![headers
        .iter()
        .map(|h| h.as_str())
        .collect::<Vec<_>>()
        .join(",")];
    for row in rows {
        let line: Vec<String> = headers
            .iter()
            .map(|h| {
                let s = cell(row.get(h.as_str()));
                if s.contains([',', '"', '\n', '\r']) {
                    format!("\"{}\"", s.replace('"', "\"\""))
                } else {
                    s
                }
            })
            .collect();
        lines.push(line.join(","));
    }
    lines.join("\n")
}

/// `<?xml ...?>` + `<records><record><key>value</key>...`; escapes `& < > "` only.
#[must_use]
pub fn format_xml(rows: &[Value]) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<records>\n");
    for row in rows {
        out.push_str("  <record>\n");
        if let Value::Object(m) = row {
            for (k, v) in m {
                let escaped = cell(Some(v))
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;")
                    .replace('"', "&quot;");
                let _ = writeln!(out, "    <{k}>{escaped}</{k}>");
            }
        }
        out.push_str("  </record>\n");
    }
    out.push_str("</records>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn node_parity() {
        let rows = [
            json!({"a":1,"b":"x,y","c":null,"d":{"k":1}}),
            json!({"a":2}),
        ];
        assert_eq!(
            format_csv(&rows),
            "a,b,c,d\n1,\"x,y\",,\"{\"\"k\"\":1}\"\n2,,,"
        );
        assert!(format_xml(&rows).contains("<d>{&quot;k&quot;:1}</d>"));
    }
}
