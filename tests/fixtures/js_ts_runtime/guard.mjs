import assert from "node:assert/strict";
import fs from "node:fs";
import net from "node:net";
import { createRequire } from "node:module";
import { spawnSync } from "node:child_process";
const request = JSON.parse(fs.readFileSync(process.argv[2], "utf8"));
const ts = createRequire(import.meta.url)(request.compiler);
assert.equal(process.version, "v22.22.2");
assert.equal(ts.version, "5.9.3");
fs.writeFileSync(request.ready, "ready");
const byte = Buffer.alloc(1);
assert.equal(fs.readSync(0, byte, 0, 1, null), 1);
const file = ts.createSourceFile("fixture.ts", "export function f(): number { return 1; }", ts.ScriptTarget.Latest, true);
assert.equal(file.parseDiagnostics.length, 0);
assert.equal(file.statements.length, 1);
assert.throws(() => fs.writeFileSync(request.outside, "changed"), error => ["EPERM", "EACCES"].includes(error.code));
const child = spawnSync("/usr/bin/false");
assert.ok(["EPERM", "EACCES"].includes(child.error?.code));
await new Promise((resolve, reject) => {
  const socket = net.connect(request.port, "127.0.0.1");
  socket.once("connect", () => { socket.destroy(); reject(new Error("network unexpectedly allowed")); });
  socket.once("error", error => {
    if (["EPERM", "EACCES"].includes(error.code)) resolve();
    else reject(error);
  });
});
fs.writeFileSync(request.done, "parsed and denied network, execution and outside write");
assert.equal(fs.readSync(0, byte, 0, 1, null), 1);
