// The markdown a note is written in: CommonMark with GitHub's additions
// (tables, task lists, ~~strike~~, bare links), plus two from Obsidian:
// [[wikilinks]] to other notes and ==highlights==. No sub/superscript: a
// "~5 min" should stay a "~5 min".
import { Language } from "@codemirror/language";
import { commonmarkLanguage } from "@codemirror/lang-markdown";
import { GFM, type MarkdownConfig, type MarkdownParser } from "@lezer/markdown";
import { tags } from "@lezer/highlight";

/** `[[Target]]`, `[[Target|shown as]]`, `[[Target#Heading]]`: one line, no brackets inside. */
const WikiLink: MarkdownConfig = {
  defineNodes: [
    { name: "WikiLink", style: tags.link },
    { name: "WikiLinkMark", style: tags.processingInstruction },
  ],
  parseInline: [
    {
      name: "WikiLink",
      parse(cx, next, pos) {
        if (next !== 91 /* [ */ || cx.char(pos + 1) !== 91) return -1;
        let end = pos + 2;
        for (; end < cx.end; end++) {
          const c = cx.char(end);
          if (c === 93 /* ] */) break;
          if (c === 10 || c === 91) return -1;
        }
        if (end + 1 >= cx.end || cx.char(end + 1) !== 93 || end === pos + 2) return -1;
        return cx.addElement(cx.elt("WikiLink", pos, end + 2, [cx.elt("WikiLinkMark", pos, pos + 2), cx.elt("WikiLinkMark", end, end + 2)]));
      },
      before: "Link",
    },
  ],
};

const Punctuation = /[!"#$%&'()*+,\-./:;<=>?@[\\\]^_`{|}~\xA1‐-‧]/;
const HighlightDelim = { resolve: "Highlight", mark: "HighlightMark" };

/** `==marked text==`. */
const Highlight: MarkdownConfig = {
  defineNodes: [
    { name: "Highlight", style: { "Highlight/...": tags.special(tags.content) } },
    { name: "HighlightMark", style: tags.processingInstruction },
  ],
  parseInline: [
    {
      name: "Highlight",
      parse(cx, next, pos) {
        if (next !== 61 /* = */ || cx.char(pos + 1) !== 61 || cx.char(pos + 2) === 61) return -1;
        const before = cx.slice(pos - 1, pos);
        const after = cx.slice(pos + 2, pos + 3);
        const sBefore = /\s|^$/.test(before);
        const sAfter = /\s|^$/.test(after);
        const pBefore = Punctuation.test(before);
        const pAfter = Punctuation.test(after);
        return cx.addDelimiter(HighlightDelim, pos, pos + 2, !sAfter && (!pAfter || sBefore || pBefore), !sBefore && (!pBefore || sAfter || pAfter));
      },
      after: "Emphasis",
    },
  ],
};

/** The parser (also used outside an editor, e.g. to find a note's headings). */
export const noteParser: MarkdownParser = (commonmarkLanguage.parser as MarkdownParser).configure([GFM, WikiLink, Highlight]);

/**
 * The language. It shares the markdown language's data, so lang-markdown's
 * list and quote commands (Enter, Backspace) treat it as markdown.
 */
export const noteLanguage = new Language(commonmarkLanguage.data, noteParser, [], "markdown");
