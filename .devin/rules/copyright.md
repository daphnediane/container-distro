---
trigger: glob
description: Copyright notice requirements for source files
globs: "**/*.rs"
---

# Copyright Header

Include at the top of all source files:

```
/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */
```

Prefer C-style block comments if supported by the language. Place before any code or imports.

## Exceptions (no header needed)

- Config files (`.toml`, `.json`, `.yaml`)
- Generated files (`Cargo.lock`, build outputs)
- Documentation (`.md`)
- Test fixtures
- Templates (unless substantially original content)

Full license text is in the root `LICENSE` file.
