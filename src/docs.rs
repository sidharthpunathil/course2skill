//! Converts chapter handouts (PDF, DOCX, PPTX, XLSX, text) to markdown.

use anyhow::{Context, Result, bail};
use calamine::{Data, Reader, open_workbook_auto};
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

pub fn convert(doc: &Path) -> Result<String> {
    let ext = doc
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let body = match ext.as_str() {
        "pdf" => tool_output(
            Command::new("pdftotext")
                .args(["-layout", "-enc", "UTF-8"])
                .arg(doc)
                .arg("-"),
            "pdftotext",
        )?,
        "docx" | "pptx" => tool_output(
            Command::new("pandoc")
                .args(["-t", "gfm", "--wrap=none"])
                .arg(doc),
            "pandoc",
        )?,
        "xlsx" => xlsx_to_md(doc)?,
        "txt" | "md" => std::fs::read_to_string(doc)?,
        other => bail!("unsupported handout type .{other}"),
    };
    let name = crate::scan::file_name(doc);
    Ok(format!(
        "---\ntitle: {name:?}\nsource: {:?}\nkind: handout\n---\n\n# {name}\n\n{}\n",
        doc.display().to_string(),
        body.trim_end()
    ))
}

fn tool_output(cmd: &mut Command, tool: &str) -> Result<String> {
    let out = cmd
        .output()
        .with_context(|| format!("could not start {tool} (is it installed?)"))?;
    if !out.status.success() {
        bail!(
            "{tool} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn xlsx_to_md(path: &Path) -> Result<String> {
    let mut wb = open_workbook_auto(path).with_context(|| format!("opening {}", path.display()))?;
    let mut md = String::new();
    for name in wb.sheet_names() {
        let range = wb
            .worksheet_range(&name)
            .with_context(|| format!("reading sheet {name}"))?;
        let rows: Vec<Vec<String>> = range
            .rows()
            .map(|r| r.iter().map(cell_text).collect())
            .collect();
        let _ = writeln!(md, "## Sheet: {name}\n");
        let table = rows_to_table(&rows);
        md.push_str(if table.is_empty() {
            "_(empty sheet)_\n"
        } else {
            &table
        });
        md.push('\n');
    }
    Ok(md)
}

fn cell_text(d: &Data) -> String {
    match d {
        Data::Empty => String::new(),
        Data::Float(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", *f as i64),
        other => other.to_string(),
    }
}

/// Data rows kept per sheet before the rest are omitted.
const MAX_ROWS: usize = 150;

/// Renders rows as a GFM table. The first non-empty row is the header.
pub fn rows_to_table(rows: &[Vec<String>]) -> String {
    let rows: Vec<&Vec<String>> = rows
        .iter()
        .filter(|r| r.iter().any(|c| !c.trim().is_empty()))
        .collect();
    let width = rows
        .iter()
        .map(|r| {
            r.iter()
                .rposition(|c| !c.trim().is_empty())
                .map_or(0, |i| i + 1)
        })
        .max()
        .unwrap_or(0);
    if rows.is_empty() || width == 0 {
        return String::new();
    }
    let cell = |r: &Vec<String>, i: usize| -> String {
        r.get(i).map_or(String::new(), |c| {
            c.trim().replace('|', "\\|").replace(['\n', '\r'], " ")
        })
    };
    let render = |r: &Vec<String>| -> String {
        let cells: Vec<String> = (0..width).map(|i| cell(r, i)).collect();
        format!("| {} |\n", cells.join(" | "))
    };
    let mut md = render(rows[0]);
    let _ = writeln!(md, "|{}", "---|".repeat(width));
    // Templates ship thousands of identical placeholder rows: collapse runs, cap the rest.
    let mut emitted = 0;
    let mut i = 1;
    while i < rows.len() {
        let line = render(rows[i]);
        let mut run = 1;
        while i + run < rows.len() && render(rows[i + run]) == line {
            run += 1;
        }
        if emitted >= MAX_ROWS {
            let left = rows.len() - i;
            let _ = writeln!(md, "\n_({left} more rows omitted, see the original file)_");
            break;
        }
        md.push_str(&line);
        emitted += 1;
        if run > 1 {
            let _ = writeln!(
                md,
                "| _(same row repeated {} more times)_ |{}",
                run - 1,
                " |".repeat(width - 1)
            );
        }
        i += run;
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn table_escapes_pipes_and_trims_empty() {
        let t = rows_to_table(&[
            row(&["A", "B", ""]),
            row(&["", "", ""]),
            row(&["x|y", "z", ""]),
        ]);
        assert_eq!(t, "| A | B |\n|---|---|\n| x\\|y | z |\n");
    }

    #[test]
    fn identical_rows_collapse() {
        let rows = vec![
            row(&["H", "I"]),
            row(&["No", "0"]),
            row(&["No", "0"]),
            row(&["No", "0"]),
        ];
        assert_eq!(
            rows_to_table(&rows),
            "| H | I |\n|---|---|\n| No | 0 |\n| _(same row repeated 2 more times)_ | |\n"
        );
    }

    #[test]
    fn long_sheets_are_capped() {
        let mut rows = vec![row(&["H"])];
        rows.extend((0..400).map(|i| row(&[&i.to_string()])));
        let t = rows_to_table(&rows);
        assert!(t.contains("250 more rows omitted"), "{t}");
    }

    #[test]
    fn empty_rows_give_empty_table() {
        assert_eq!(rows_to_table(&[row(&["", " "])]), "");
    }
}
