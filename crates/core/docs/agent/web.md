# Web search and fetching
**`web_search`** tries providers in order and falls back automatically when one fails, rate-limits or finds nothing:
- `[web] search` picks the first (`auto`, `searxng`, `brave`, `tavily`, `duckduckgo`), then `chain` (default `searxng, brave, tavily, duckduckgo`).
- SearXNG needs `searxng_url`; Brave and Tavily need an API key, stored with the provider credentials (the apps' web search settings, or the `set_search_key` request). Keys never reach the model and are scrubbed from tool output.
- A provider that rate-limited is tried last for a while. When a fallback answered, the result starts with a short note on what failed.

**`web_fetch`** returns a page as readable text; page through long documents with `offset`. Give `prompt` (a question) to get only a focused answer instead of the page: a helper model (the `fast`/utility role; a private chat's own model) reads the page and answers from it with key quotes and the URL, and the full text is kept as an artifact (`recall artifact=…`). Pages under ~6k characters come back whole. Prefer `prompt` for "find X on this page" to save context; fetch without it when you need exact wording.

Invisible characters (tag characters, bidi controls, zero-width runs) are removed from fetched pages. Local and private network addresses need the user's approval.

Claude Code uses configured Tessera `web_search` instead of CLI WebSearch; otherwise CLI search stays. Native `web_fetch` is bridged alongside CLI WebFetch (different extraction/paging). Private Claude chats bridge selected public search only, not fetch.
