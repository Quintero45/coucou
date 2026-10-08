// A small syntax colouring for the editor view: one line at a time, by file
// extension. Keywords, strings, numbers, comments, calls and types — enough to
// read code at a glance, no grammar. A string or comment that spans lines is
// coloured only on its first line.

import { h } from "./dom";

const KEYWORDS = new Set(
  (
    "abstract as async await break case catch class const continue crate def default defer del delete do dyn elif " +
    "else enum except export extends false finally fn for from func function go if impl implements import in " +
    "instanceof interface is let loop match mod module mut namespace new nil None not null of or package pass " +
    "private protected pub public raise readonly ref return self Self static struct super switch this throw " +
    "trait True true try type typeof undefined unsafe use var void where while with yield and lambda elif " +
    "global nonlocal assert fun val when object override sealed internal echo function local then end"
  ).split(" "),
);

/** Families whose line comments start with "#". */
const HASH = new Set(["py", "rb", "sh", "bash", "zsh", "ps1", "psm1", "yaml", "yml", "toml", "r", "pl", "conf", "ini", "dockerfile", "mk"]);
/** No colouring at all: prose and data that would only look noisy. */
const PLAIN = new Set(["md", "markdown", "txt", "log", "csv", "tsv", "svg", "xml", "html", "htm", "lock"]);

type Kind = "kw" | "str" | "num" | "com" | "fn" | "type";

export function languageOf(path: string): string {
  const name = path.split(/[\\/]/).pop()?.toLowerCase() ?? "";
  if (name === "dockerfile" || name === "makefile") return name === "makefile" ? "mk" : "dockerfile";
  const dot = name.lastIndexOf(".");
  return dot >= 0 ? name.slice(dot + 1) : "";
}

/** One line as coloured spans (plain text in between). */
export function highlightLine(text: string, lang: string): (Node | string)[] {
  return tokenize(text, lang).map((tok) => (tok.kind ? h("span", { class: `hl-${tok.kind}`, text: tok.text }) : tok.text));
}

/** One line cut into pieces, each with its colour (null: plain). */
export function tokenize(text: string, lang: string): { kind: Kind | null; text: string }[] {
  if (PLAIN.has(lang) || text.length > 400) return [{ kind: null, text }];
  const comment = HASH.has(lang) ? "#[^\\n]*" : lang === "sql" || lang === "lua" ? "--[^\\n]*" : "\\/\\/[^\\n]*|\\/\\*.*?(?:\\*\\/|$)";
  const re = new RegExp(
    `(${comment})|("(?:[^"\\\\]|\\\\.)*"?|'(?:[^'\\\\]|\\\\.)*'?|\`(?:[^\`\\\\]|\\\\.)*\`?)` +
      `|(\\b0x[0-9a-fA-F_]+\\b|\\b\\d[\\d_]*(?:\\.\\d+)?(?:[eE][+-]?\\d+)?\\b)|([A-Za-z_$][\\w$]*)`,
    "g",
  );
  const out: { kind: Kind | null; text: string }[] = [];
  let last = 0;
  const push = (kind: Kind | null, value: string) => out.push({ kind, text: value });
  for (const m of text.matchAll(re)) {
    const at = m.index ?? 0;
    if (at > last) push(null, text.slice(last, at));
    const [whole, com, str, num, word] = m;
    if (com) push("com", com);
    else if (str) push("str", str);
    else if (num) push("num", num);
    else if (word) {
      if (KEYWORDS.has(word)) push("kw", word);
      else if (/^\s*\(/.test(text.slice(at + word.length))) push("fn", word);
      else if (/^[A-Z][a-z0-9]/.test(word)) push("type", word);
      else push(null, word);
    }
    last = at + whole.length;
  }
  if (last < text.length) push(null, text.slice(last));
  return out;
}
