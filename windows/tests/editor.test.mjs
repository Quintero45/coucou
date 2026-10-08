// The editor view's rows: real line numbers from the file's context when the
// edit was found there, none otherwise; and the line colouring.

import { test } from "node:test";
import assert from "node:assert/strict";
import { editorRows, lookupOf } from "../src/views/editor.ts";
import { languageOf, tokenize } from "../src/views/highlight.ts";
import { buildFileDiff, fromEdit, fromNew } from "../src/core/diff.ts";

const edit = fromEdit("const IVA = 0.16;\n", "const IVA = 0.19;\nconst DESCUENTO = 0.05;\n", "C:\\p\\factura.ts");

test("an edit found in its file gets the file's line numbers and neighbours", () => {
  const rows = editorRows(edit, [{ line: 3, before: ["import x;", ""], after: ["", "export function total() {"] }]);
  assert.deepEqual(rows.map((r) => [r.kind, r.num]), [
    ["ctx", 1], ["ctx", 2], ["del", 3], ["add", 3], ["add", 4], ["ctx", 5], ["ctx", 6],
  ]);
  assert.equal(rows[2].text, "const IVA = 0.16;");
});

test("not found: the edit alone, without numbers", () => {
  const rows = editorRows(edit, [null]);
  assert.deepEqual(rows.map((r) => [r.kind, r.num]), [["del", null], ["add", null], ["add", null]]);
});

test("the file is searched for the whole edit, so short changes still get numbers", () => {
  const d = fromEdit("a\nb\nc\nd\n0.16\ne\nf\ng\nh\n", "a\nb\nc\nd\n0.19\ne\nf\ng\nh\n", "C:\\p\\x.ts");
  const look = lookupOf(d, d.hunks[0]);
  assert.equal(look.text, "a\nb\nc\nd\n0.19\ne\nf\ng\nh\n");
  // The hunk shows lines 2–8 of the edit: context b c d, then 0.19, then e f g.
  assert.deepEqual([look.from, look.to], [2, 8]);
  const multi = buildFileDiff("Edit", {
    file_path: "C:\\p\\x.ts",
    edits: [{ old_string: "x", new_string: "y" }, { old_string: "p\nq", new_string: "p\nr" }],
  });
  assert.deepEqual(multi.hunks.map((h) => lookupOf(multi, h)), [
    { text: "y", from: 1, to: 1 }, { text: "p\nr", from: 1, to: 2 },
  ]);
  const bare = { ...d, edits: undefined };
  assert.deepEqual(lookupOf(bare, d.hunks[0]), { text: "b\nc\nd\n0.19\ne\nf\ng", from: 1, to: 7 });
});

test("a new file is numbered from 1 without asking", () => {
  const rows = editorRows(fromNew("a\nb\n", "C:\\p\\n.ts"), undefined);
  assert.deepEqual(rows.map((r) => [r.kind, r.num]), [["add", 1], ["add", 2]]);
});

test("colouring: keywords, strings, numbers, calls, types and comments", () => {
  const line = 'const total = sum(Item, "x", 42); // ok';
  const toks = tokenize(line, "ts");
  assert.equal(toks.map((x) => x.text).join(""), line);
  assert.deepEqual(toks.filter((x) => x.kind).map((x) => [x.kind, x.text]), [
    ["kw", "const"], ["fn", "sum"], ["type", "Item"], ["str", '"x"'], ["num", "42"], ["com", "// ok"],
  ]);
  assert.deepEqual(tokenize("# title", "md"), [{ kind: null, text: "# title" }]);
  assert.equal(tokenize("x = 1 # note", "py").at(-1).kind, "com");
  assert.equal(languageOf("C:\\a\\b.test.TS"), "ts");
});
