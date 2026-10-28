import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { fileURLToPath } from "node:url";
import { collectCandidates } from "./collect_candidates.mjs";
const bundle = path.dirname(fileURLToPath(import.meta.url));
const ts = (await import(path.join(bundle, "node_modules/typescript/lib/typescript.js"))).default;
const request = JSON.parse(fs.readFileSync(0, "utf8"));
const root = fs.realpathSync(request.root);
const tools = fs.realpathSync(path.dirname(ts.sys.getExecutingFilePath()));
const inside = (p, base) => p === base || p.startsWith(base + path.sep);
const permitted = (p) => inside(path.resolve(p), root) || inside(path.resolve(p), tools);
const inputs = new Map();
let total = 0;
const hostFailures = [];
function guarded(operation) {
  return (...args) => {
    try { return operation(...args); }
    catch (error) { hostFailures.push(String(error)); throw error; }
  };
}
function verifyHost() {
  if (hostFailures.length) throw new Error("compiler host failure: " + hostFailures.join("; "));
}
function readFile(p) {
  if (!permitted(p)) throw new Error("undeclared content read: " + p);
  if (!fs.existsSync(p)) return undefined;
  const real = fs.realpathSync(p);
  if (!permitted(real)) throw new Error("undeclared symlink content: " + p);
  const data = fs.readFileSync(real);
  if (data.length > 8 * 1024 * 1024) throw new Error("oversized input");
  if (!inputs.has(real)) total += data.length;
  if (total > 512 * 1024 * 1024) throw new Error("input budget exceeded");
  const pin = { sha256: crypto.createHash("sha256").update(data).digest("hex"), bytes: data.length };
  if (inputs.has(real) && inputs.get(real).sha256 !== pin.sha256) throw new Error("input changed between reads");
  if (!inputs.has(real)) inputs.set(real, pin);
  const text = data.toString("utf8");
  if (!Buffer.from(text, "utf8").equals(data)) throw new Error("input must be valid UTF-8");
  return text;
}
const configHost = {
  useCaseSensitiveFileNames: true,
  readFile,
  fileExists: p => permitted(p) && ts.sys.fileExists(p),
  readDirectory: (p, ...args) => { if (!permitted(p)) throw new Error("undeclared directory read"); return ts.sys.readDirectory(p, ...args); },
};
if (typeof request.project !== "string" || path.isAbsolute(request.project)) throw new Error("project must be relative to source");
const project = path.resolve(root, request.project);
if (!inside(project, root)) throw new Error("project escapes source");
for (const key of ["readFile", "fileExists", "readDirectory"]) configHost[key] = guarded(configHost[key]);
const loaded = ts.readConfigFile(project, configHost.readFile);
verifyHost();
if (loaded.error) throw new Error(ts.flattenDiagnosticMessageText(loaded.error.messageText, "\n"));
const config = ts.parseJsonConfigFileContent(loaded.config, configHost, path.dirname(project), undefined, project);
verifyHost();
if (config.errors.length || config.projectReferences?.length) throw new Error("invalid or referenced project");
const options = { ...config.options, noEmit: true };
const host = ts.createCompilerHost(options, true);
host.readFile = readFile;
host.fileExists = configHost.fileExists;
host.directoryExists = p => permitted(p) && ts.sys.directoryExists(p);
host.getDirectories = p => permitted(p) ? ts.sys.getDirectories(p) : [];
host.realpath = p => { if (!permitted(p)) throw new Error("undeclared realpath"); const real = ts.sys.realpath(p); if (!permitted(real)) throw new Error("undeclared alias"); return real; };
host.getCurrentDirectory = () => root;
host.writeFile = () => { throw new Error("emit forbidden"); };
for (const key of ["readFile", "fileExists", "directoryExists", "getDirectories", "realpath", "writeFile"]) host[key] = guarded(host[key]);
const program = ts.createProgram(config.fileNames, options, host);
const { diagnostics, sites } = collectCandidates({ ts, program, options, root, tools, reportFiles: request.report_files, verifyHost });
verifyHost();
for (const [p, pin] of inputs) {
  if (fs.realpathSync(p) !== p) throw new Error("input alias drift");
  if (crypto.createHash("sha256").update(fs.readFileSync(p)).digest("hex") !== pin.sha256) throw new Error("input drift");
}
console.log(JSON.stringify({ compiler: ts.version, status: "candidate evidence only; no canonical edge admission", diagnostics, inputs: Object.fromEntries([...inputs].sort()), sites }, null, 2));
