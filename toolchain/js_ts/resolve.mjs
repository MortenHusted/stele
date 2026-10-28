import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import ts from "./typescript.cjs";
import { collectCandidates } from "./collect_candidates.mjs";

const root = "/__codemap__/source";
const tools = "/__codemap__/typescript/lib";
const compare = (a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b));
const roots = new Map([["{source}", root], ["{typescript}", tools]]);
const inside = (p, base) => p === base || p.startsWith(base + "/");
const permitted = p => inside(p, root) || inside(p, tools);
const excluded = p => p === root + "/.git" || p.startsWith(root + "/.git/");
function virtual(logical) {
  for (const [name, directory] of roots) {
    if (logical === name) return directory;
    if (logical.startsWith(name + "/")) {
      const parts = logical.slice(name.length + 1).split("/");
      if (parts.some(p => !p || p === "." || p === ".." || /[\\\0]/.test(p))) throw new Error("invalid logical input path");
      return directory + "/" + parts.join("/");
    }
  }
  throw new Error("undeclared logical input root");
}
function logical(p) {
  for (const [name, directory] of roots) {
    if (p === directory) return name;
    if (p.startsWith(directory + "/")) return name + p.slice(directory.length);
  }
  return "@outside/" + p;
}
function control() {
  const byte = Buffer.alloc(1);
  if (fs.readSync(0, byte, 0, 1, null) !== 1) throw new Error("parent control closed");
}
function frame(value) { fs.writeSync(1, JSON.stringify(value) + "\n"); }
if (process.version !== "v22.22.2" || ts.version !== "5.9.3") throw new Error("unpinned runtime/compiler version");
frame({ phase: "ready", node: process.version, typescript: ts.version });
control();
const request = JSON.parse(fs.readFileSync(process.argv[2], "utf8"));
if (request.schema !== "js-ts-helper/1") throw new Error("unknown helper request schema");
const origins = Object.entries(request.origins || {}).map(([name, origin]) => [origin, roots.get("{" + name + "}")]);
function normalize(p) {
  p = path.posix.resolve(p);
  for (const [origin, destination] of origins) {
    if (!destination || !path.posix.isAbsolute(origin)) throw new Error("invalid input origin");
    if (inside(p, origin)) return destination + p.slice(origin.length);
  }
  return p;
}
const files = new Map(Object.entries(request.data.files).map(([name, text]) => [virtual(name), text]));
const directories = new Map(Object.entries(request.data.directories).map(([name, entries]) => [virtual(name), new Map(Object.entries(entries))]));
const aliases = new Map();
for (const [directory, entries] of directories) {
  for (const [name, entry] of entries) {
    if (!name || name === "." || name === ".." || /[\/\\\0]/.test(name)) throw new Error("invalid directory entry");
    if (!["file", "directory"].includes(entry.kind)) throw new Error("invalid directory entry kind");
    if (entry.target !== undefined) aliases.set(directory + "/" + name, virtual(entry.target));
  }
}
const hostFailures = [];
const inputs = new Map();
const lookups = new Map();
function guarded(operation) {
  return (...args) => {
    try { return operation(...args); }
    catch (error) { hostFailures.push(String(error)); throw error; }
  };
}
function verifyHost() { if (hostFailures.length) throw new Error("compiler host failure: " + hostFailures.join("; ")); }
function resolveAlias(p) {
  p = normalize(p);
  const seen = new Set();
  while (true) {
    if (!permitted(p) || excluded(p)) throw new Error("undeclared realpath");
    if (seen.has(p)) throw new Error("cyclic captured alias");
    seen.add(p);
    let cursor = p;
    let match;
    while (permitted(cursor)) {
      if (aliases.has(cursor)) { match = cursor; break; }
      cursor = path.posix.dirname(cursor);
    }
    if (!match) return p;
    p = aliases.get(match) + p.slice(match.length);
  }
}
function record(operation, query, resolved, result) {
  const value = { operation, path: logical(query), resolved: logical(resolved), result };
  lookups.set(JSON.stringify([operation, value.path]), value);
  return result;
}
function entries(p) {
  const query = normalize(p);
  p = resolveAlias(query);
  const found = directories.get(p);
  if (!found) throw new Error("undeclared directory lookup: " + logical(p));
  record("directory_entries", query, p, "captured");
  return found;
}
function metadata(p) {
  const query = normalize(p);
  if (!permitted(query)) {
    if (["/__codemap__/node_modules", "/node_modules"].some(base => inside(query, base)))
      return record("metadata", query, query, "virtual_ancestor_package_absent");
    const virtualPeerPackageFallback = root.slice(0, "node_modules".length - 1);
    if (inside(query, virtualPeerPackageFallback))
      return record("metadata", query, query, "virtual_compiler_peer_package_absent");
    throw new Error("undeclared metadata lookup: " + query);
  }
  p = resolveAlias(query);
  if (directories.has(p)) return record("metadata", query, p, "directory");
  const parent = path.posix.dirname(p);
  if (metadata(parent) !== "directory") return record("metadata", query, p, "absent_parent");
  const entry = entries(parent).get(path.posix.basename(p));
  return record("metadata", query, p, entry?.kind || "absent");
}
const readFile = guarded(p => {
  const query = normalize(p);
  if (!permitted(query) || excluded(query)) throw new Error("undeclared content read");
  p = resolveAlias(query);
  if (metadata(p) !== "file") return undefined;
  const text = files.get(p);
  if (text === undefined) throw new Error("uncaptured content read: " + logical(p));
  const bytes = Buffer.from(text, "utf8");
  if (bytes.length > 8 * 1024 * 1024) throw new Error("oversized captured input");
  const pin = { sha256: crypto.createHash("sha256").update(bytes).digest("hex"), bytes: bytes.length };
  inputs.set(logical(p), pin);
  record("read_file", query, p, pin.sha256);
  return text;
});
const fileExists = guarded(p => metadata(p) === "file");
const directoryExists = guarded(p => metadata(p) === "directory");
const getDirectories = guarded(p => {
  if (!permitted(normalize(p))) { metadata(p); return []; }
  return [...entries(p)].filter(([, e]) => e.kind === "directory").map(([n]) => n).sort();
});
const realpath = guarded(resolveAlias);
const readDirectory = guarded((p, extensions, excludes, includes, depth) => ts.matchFiles(
  p, extensions, excludes, includes, true, root, depth,
  directory => {
    const listing = [...entries(directory)];
    return { files: listing.filter(([, e]) => e.kind === "file").map(([n]) => n),
      directories: listing.filter(([, e]) => e.kind === "directory").map(([n]) => n) };
  }, realpath
));
if (typeof request.project !== "string" || !request.project || path.posix.isAbsolute(request.project)
    || request.project.split("/").some(p => !p || p === "." || p === ".." || /[\\\0]/.test(p))) throw new Error("invalid relative project path");
const project = root + "/" + request.project;
const configHost = { useCaseSensitiveFileNames: true, readFile, fileExists, readDirectory };
const loaded = ts.readConfigFile(project, readFile);
verifyHost();
if (loaded.error) throw new Error(ts.flattenDiagnosticMessageText(loaded.error.messageText, "\n"));
const config = ts.parseJsonConfigFileContent(loaded.config, configHost, path.posix.dirname(project), undefined, project);
verifyHost();
if (config.errors.length || config.projectReferences?.length) throw new Error("invalid or referenced project");
if (config.options.plugins?.length) throw new Error("compiler plugins are unsupported");
const options = { ...config.options, noEmit: true };
function configuredPath(value) {
  const p = normalize(value);
  if (!permitted(p) || excluded(p)) throw new Error("configured path outside captured roots");
  return p;
}
for (const descriptor of ts.optionDeclarations) {
  const value = options[descriptor.name];
  if (descriptor.isFilePath && typeof value === "string") options[descriptor.name] = configuredPath(value);
  if (descriptor.type === "list" && descriptor.element?.isFilePath && Array.isArray(value)) options[descriptor.name] = value.map(configuredPath);
}
for (const name of ["configFilePath", "pathsBasePath"]) if (typeof options[name] === "string") options[name] = configuredPath(options[name]);
if (options.paths) {
  const mappingBase = ts.getPathsBasePath(options, { getCurrentDirectory: () => root });
  options.paths = Object.fromEntries(Object.entries(options.paths).map(([name, paths]) => [name, paths.map(p => {
    const resolved = configuredPath(path.posix.resolve(mappingBase, p));
    return path.posix.isAbsolute(p) ? resolved : p;
  })]));
}
const host = ts.createCompilerHost(options, true);
Object.assign(host, { useCaseSensitiveFileNames: () => true, getCanonicalFileName: p => p, readFile, fileExists, directoryExists, getDirectories, realpath,
  getCurrentDirectory: () => root, getDefaultLibLocation: () => tools,
  getDefaultLibFileName: opts => tools + "/" + path.posix.basename(ts.getDefaultLibFilePath(opts)),
  writeFile: guarded(() => { throw new Error("emit forbidden"); }) });
const program = ts.createProgram(config.fileNames.map(configuredPath), options, host);
const evidence = collectCandidates({ ts, program, options, root, tools, reportFiles: request.report_files, verifyHost });
verifyHost();
const programFiles = program.getSourceFiles().map(file => ({ path: logical(file.fileName), declaration: file.isDeclarationFile,
  common_js: !!file.commonJsModuleIndicator || /\.c(js|ts)$/.test(file.fileName) || file.impliedNodeFormat === ts.ModuleKind.CommonJS || options.module === ts.ModuleKind.CommonJS })).sort((a, b) => compare(a.path, b.path));
evidence.sites.sort((a, b) => compare(a.location.path, b.location.path) || a.location.start_byte - b.location.start_byte);
frame({ phase: "result", schema: "js-ts-helper/1", compiler: ts.version, node: process.version,
  project: request.project, options, program_files: programFiles,
  inputs: Object.fromEntries([...inputs].sort()), lookups: [...lookups.entries()].sort().map(([, v]) => v),
  sites: evidence.sites, aliases: evidence.aliases, program_diagnostics: evidence.program_diagnostics, diagnostics: evidence.diagnostics.map(({text, ...diagnostic}) => diagnostic) });
control();
