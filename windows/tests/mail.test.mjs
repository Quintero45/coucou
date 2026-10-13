// The mail card (views/mail.ts): mail.rs's error codes in words, file sizes,
// and the choose card's way in.

import { test } from "node:test";
import assert from "node:assert/strict";
import { fileSize, mailError } from "../src/views/mail.ts";

test("mail.rs error codes read as sentences", () => {
  assert.equal(mailError("no_recipient"), "Add at least one recipient.");
  assert.equal(mailError("Error: bad_address:juan@"), "juan@ is not a valid address.");
  assert.equal(mailError("file_too_big"), "The file is over 20 MB.");
  assert.match(mailError("no_mail"), /nothing was sent/);
  assert.equal(mailError("outlook:timeout"), "Outlook: timeout");
  assert.equal(mailError("resend:HTTP 403"), "Resend: HTTP 403");
  assert.equal(mailError("something else"), "something else");
});

test("attachment sizes", () => {
  assert.equal(fileSize(10), "1 KB");
  assert.equal(fileSize(340 * 1024), "340 KB");
  assert.equal(fileSize(1.25 * 1024 * 1024), "1.3 MB");
});
