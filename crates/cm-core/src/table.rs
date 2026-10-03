/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Small helpers for rendering plain-text tables.

/// Format a byte count the way `container` does: binary units, one
/// decimal place below 10, whole numbers above (`32G`, `75M`, `1.5G`).
#[must_use]
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "K", "M", "G", "T", "P"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 || value >= 10.0 || value.fract() < 0.05 {
        format!("{}{}", value.round() as u64, UNITS[unit])
    } else {
        format!("{value:.1}{}", UNITS[unit])
    }
}

/// Upper-case the first character (`running` → `Running`).
#[must_use]
pub fn title_case(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Render rows as left-aligned columns separated by `gap` spaces. The last
/// column is not padded.
#[must_use]
pub fn columns(rows: &[Vec<String>], gap: usize) -> Vec<String> {
    let ncols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..ncols)
        .map(|c| {
            rows.iter()
                .filter_map(|r| r.get(c))
                .map(|s| s.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    rows.iter()
        .map(|row| {
            let mut line = String::new();
            for (c, cell) in row.iter().enumerate() {
                if c + 1 == row.len() {
                    line.push_str(cell);
                } else {
                    let pad = widths[c] + gap - cell.chars().count();
                    line.push_str(cell);
                    line.extend(std::iter::repeat_n(' ', pad));
                }
            }
            line
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_matches_container() {
        assert_eq!(human_bytes(34359738368), "32G");
        assert_eq!(human_bytes(78872576), "75M");
        assert_eq!(human_bytes(1536 * 1024 * 1024), "1.5G");
        assert_eq!(human_bytes(512), "512B");
    }

    #[test]
    fn title_case_first_char() {
        assert_eq!(title_case("running"), "Running");
        assert_eq!(title_case(""), "");
    }

    #[test]
    fn columns_pad_all_but_last() {
        let rows = vec![
            vec!["a".to_string(), "bb".to_string(), "c".to_string()],
            vec!["aaa".to_string(), "b".to_string(), "c".to_string()],
        ];
        assert_eq!(columns(&rows, 2), ["a    bb  c", "aaa  b   c"]);
    }
}
