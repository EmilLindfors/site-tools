# site-tools

CLI for lindfors.no blog tasks: citation processing, newsletter generation, and PDF export.

## Build

```sh
cargo build --release
```

The binary is at `target/release/site-tools`. Run all commands from the project root.

## Commands

### cite

Process Zotero citations (`@citekey` references) in blog posts. Requires [Zotero](https://www.zotero.org/) with [Better BibTeX](https://retorque.re/zotero-better-bibtex/) installed.

The tool auto-detects Zotero's data directory. Override with `ZOTERO_DATA_DIR` env var if needed.

```sh
# List all available citekeys from Zotero/BBT
site-tools cite list

# Look up a specific citekey
site-tools cite lookup @Christiansen2017

# Process a blog post, replacing @citekeys with formatted citations
# Prints to stdout by default
site-tools cite process content/blog/aquaculture-innovation/index.md

# Write output to a file
site-tools cite process content/blog/aquaculture-innovation/index.md --output processed.md

# Use a different citation style (default: apa)
site-tools cite process content/blog/my-post/index.md --style numeric-link
```

**Citation styles:**

| Style | Inline | Parenthetical | References |
|-------|--------|---------------|------------|
| `apa` (default) | Author (Year) | (Author, Year) | Sorted alphabetically |
| `numeric` | [1] | [1] | Numbered in order of appearance |
| `numeric-link` | [1](#ref-key) | [1](#ref-key) | Numbered with anchor links |

**Markdown syntax:**

- `@Smith2020` — narrative citation: "Smith (2020) found..."
- `[@Smith2020]` — parenthetical citation: "...is well documented (Smith, 2020)"

A `## References` section is appended automatically unless one already exists.

### newsletter

Generate and send email newsletters from blog posts.

```sh
# Generate newsletter markdown from a blog post
# Output: static/newsletter/<slug>.md
site-tools newsletter gen content/blog/my-post/index.md

# Send newsletter to all subscribers
# Reads ADMIN_KEY from .env
site-tools newsletter send my-post

# Send with a custom subject line
site-tools newsletter send my-post --subject "Special edition: My Post"
```

`gen` strips shortcodes, math blocks, and other site-specific markup, then writes a clean markdown file with frontmatter (title, date, description, url).

`send` calls the site API (`/api/send-newsletter`) with the slug. Prompts for confirmation before sending. Requires `ADMIN_KEY` in a `.env` file.

### pdf

Generate PDFs from blog posts using [Typst](https://typst.app/).

```sh
# Generate PDF from a blog post
# Output: static/pdf/<slug>.pdf
site-tools pdf gen content/blog/my-post/index.md
```

This converts WebP images to PNG, preprocesses markdown for Typst compatibility (strips citation links, converts HTML references), and compiles with the `academic.typ` template. Requires `typst` on `PATH` and the template at `templates/pdf/academic.typ`.

## Dependencies

- **cite**: Zotero + Better BibTeX (reads their SQLite databases directly)
- **newsletter send**: `curl`, `.env` file with `ADMIN_KEY`
- **pdf gen**: `typst` CLI, project fonts in `fonts/`
