# site-tools

The generators behind [lindfors.no](https://lindfors.no): two Rust CLIs that produce the
derived files the site commits and serves.

They live here rather than in the site repo because they are not part of the site.
[lindfors-site](https://github.com/EmilLindfors/lindfors-site) is what Cloudflare Pages
builds — `content/`, `templates/`, `sass/`, `static/`, `zola.toml` — and it must keep
building from a plain `zola build` with nothing here installed. These tools run before
that, on a workstation or on the publishing host, and what they write is committed.

## The crates

**`crates/site-tools`** — one binary, one subcommand per artefact:

| Subcommand | What it produces |
|---|---|
| `cite` | Resolves `@key` / `[@key]` markers against crossref and stores the reference in the post's own frontmatter |
| `markdown` | The plain-markdown representation served by content negotiation, plus `llms.txt` and `posts.json` |
| `speech` / `audio` | The spoken script for a post, and its MP3 |
| `pdf` / `cv` | Per-post PDFs and the CV, through Typst |
| `og` | The 1200×630 share image |
| `hero` | Model-drawn hero images and social cards, through OpenRouter |
| `newsletter` | The issue file for a post |
| `schedule` / `publish` | Queueing a finished post, and publishing it on the day |

**`crates/img-optim`** — converts an image to WebP at the sizes the site uses. Run by
hand when an image is added; `site-tools hero` also shells out to it.

## Building

```sh
cargo build --release          # both crates
cargo test                     # the suites for both
```

`site-tools` has one feature, `cite`, on by default. It is the only thing here that
needs the network, and currently the only thing in that crate that needs a C toolchain
(crossref-client takes reqwest with default features, so rustls brings `aws-lc-sys`).
The publisher builds `--no-default-features`, which is pure Rust and cross-compiles to
`aarch64-unknown-linux-musl` with nothing but a rustup target and `rust-lld`; citations
are resolved before a post is ever queued. `img-optim` is C either way — libwebp.

## Using them from the site

Nothing in the site repo hardcodes where this is checked out. `scripts/lib.sh` there
resolves, in order:

- `SITE_TOOLS_BIN` — a built binary, used as-is and never rebuilt. This is what the
  publishing host sets, since it has no cargo.
- `SITE_TOOLS_DIR` — the crate directory, built from source.
- a conventional path, for a workstation with the repos checked out beside each other.

`img-optim` is found the same way, through `IMG_OPTIM_BIN` / `IMG_OPTIM_DIR`.

## Adapting them

`pdf.rs` is the piece most likely to be useful on its own: it takes a Zola post and
renders it through Typst, and is generic enough to adapt to another static site
generator. The [PDF post](https://lindfors.no/blog/typst-for-blogging/) walks through
it, and [the citations post](https://lindfors.no/blog/citations-on-a-static-site/)
covers `cite.rs`.

## Licence

None set yet, which means the default applies: all rights reserved. If you want to
reuse a piece of this, ask — <emil@lindfors.no>.
