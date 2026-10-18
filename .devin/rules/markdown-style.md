---
trigger: model_decision
description: Markdown conventions — ASCII-only inside tables, concise cells with detail in prose after
---

# Markdown style

## Tables

- **ASCII only inside tables.** Non-ASCII glyphs confuse IDE
  reformatters' column counting. Use `--` for em dash, `-` for en
  dash/minus, `->` for `→`, `<=` for `≤`, `...` for `…`, `+`/`-` for
  `∪`/`−`. Prose outside tables may still use Unicode normally.
- **Keep cells concise.** One concept per row; move long parenthetical
  detail into a prose paragraph after the table rather than growing a
  wide cell.
