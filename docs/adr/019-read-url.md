# 2026-10-29 — read_url: публичное чтение страниц

## Context

The chat model could search the web and research snippets, but it could not read
a specific URL that a user supplied. The handler only recognizes media links and
sends them to `yt-dlp`; an article link was either ignored in a group or passed
to the model as plain text with no way to open it. Porting all of omp's `read`
would expose local files, archives, SQLite, SSH, and internal `agent://`/`xd://`
schemes to every group member, which is unacceptable in the bot container.

## Decision

- Add a separate `read_url` tool instead of porting the universal `read` tool.
  It accepts one public `http` or `https` URL and returns extracted text.
- `ToolVisibility::All`: the page is public and the tool has no bot-local side
  effects. The model may call it at most five times per reply (`READ_URL_CALLS_PER_REPLY = 5`).
- The target must be an exact normalized URL from the current user prompt, reply
  context, recent chat messages, or links discovered by `web_search` / `research` /
  `read_url` in the current or recent turns of the same chat (`prompt_urls`/`allowed_target`,
  plus query-less public URLs for the owner). The model cannot choose an arbitrary URL and
  append chat summary or recent messages as query data; that would be egress.
  The URL fragment is ignored for comparison because it is never sent.
- Reject credentials, non-standard ports, IP literals, single-label hosts,
  `localhost`, `.local`, `.internal`, `.lan`, `.home.arpa`, private/link-local/
  loopback/reserved IPv4, and IPv6-only hosts.
- Resolve DNS, keep only permitted public IPv4 addresses, and pin them into the
  reqwest client. Redirects are followed manually, at most five, and every hop
  is validated again. No cookies, authorization headers, proxy, or browser
  session are used. The custom guard remains because the researched
  `reqwest-ssrf-guard` requires reqwest 0.13, while this project pins
  reqwest 0.12.
- Fetch at most 2 MiB, with a 15-second per-hop timeout, a 20-second total
  budget (DNS, redirects, body), and a 24,000-character output cap. The title
  is capped at 200 characters and control characters are stripped before it
  enters the header. Before parsing, a linear HTML scan rejects more than
  20,000 elements or nesting deeper than 128; this bounds CPU work from hostile
  markup. Non-prose elements are removed with `dom_query`, then
  `dom_smoothie`/readability extracts the main article and renders Markdown.
  Headers and footers are kept because they often carry the article title,
  byline, and attribution. Trailing selectors (`:1-120`, `:-80`, `:raw`) slice
  cached pages without re-fetching; recognized sites (GitHub blob/issue/PR,
  Reddit, Wikipedia, Hacker News, crates.io, PyPI) and RSS/Atom feeds use
  dedicated clean renderers (`src/read_url_sites.rs`). PDFs and other binaries
  are rejected for now.
- Text is decoded from the declared charset or detected with
  `chardetng`/`encoding_rs`, so windows-1251 and similar pages do not turn into
  mojibake.
- Cache successful pages for ten minutes, capped at sixteen entries and 64k
  characters per cached text. The cache is keyed by requested and final URL and contains
  only public data.
- Page text is labelled as untrusted data in both the tool result header and
  the system prompt. The model must not follow instructions found in a page.

## Consequences

- Group members can ask the bot to read an article, but cannot read local files,
  `.env`, cookies, the database, SSH keys, or internal services.
- `read_url` is read-only and remains available in research mode; it is not a
  way to trigger downloads or owner actions.
- PDF/DOCX conversion, JS rendering, authenticated pages, and paywalled pages
  are explicitly out of scope for this iteration. They can be added behind the
  same URL guard and size limits.
- The URL branch does not replace `yt-dlp`: supported media links still go to
  the downloader, and the model is told not to use `read_url` for media.
