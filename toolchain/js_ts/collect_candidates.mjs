import path from "node:path";
const inside = (p, base) => p === base || p.startsWith(base + path.sep);

export function collectCandidates({ ts, program, options, root, tools, reportFiles: selectedFiles, verifyHost }) {
  const checker = program.getTypeChecker();
  const fileLocations = new WeakMap();
  const diagnostics = ts.getPreEmitDiagnostics(program).map(d => ({ code: d.code, path: d.file && path.relative(root, d.file.fileName), start: d.start, length: d.length, text: ts.flattenDiagnosticMessageText(d.messageText, "\n") }));
  const writes = new Map();
  const initializedBindings = new Map();
  const writtenBindings = new Map();
  const dynamicFiles = new Set();
  function resolvedSymbol(node) {
    let symbol = ts.isShorthandPropertyAssignment(node.parent) ? checker.getShorthandAssignmentValueSymbol(node.parent) : checker.getSymbolAtLocation(node);
    if (symbol && symbol.flags & ts.SymbolFlags.Alias) symbol = checker.getAliasedSymbol(symbol);
    return symbol;
  }
  function recordWrite(node) {
    if (ts.isIdentifier(node)) {
      const symbol = resolvedSymbol(node);
      if (symbol) {
        const entries = writes.get(symbol) || [];
        entries.push(location(node));
        writes.set(symbol, entries);
        for (const declaration of symbol.declarations || []) {
          const scope = bindingScope(declaration);
          const names = writtenBindings.get(scope) || new Set();
          names.add(symbol.name);
          writtenBindings.set(scope, names);
        }
      }
    }
    ts.forEachChild(node, recordWrite);
  }
  for (const file of program.getSourceFiles()) {
    if (!inside(file.fileName, root) || file.fileName.includes("/node_modules/") || file.isDeclarationFile) continue;
    function scan(node) {
      if (ts.isVariableDeclaration(node) && (node.initializer || ((ts.isForInStatement(node.parent.parent) || ts.isForOfStatement(node.parent.parent)) && node.parent.parent.initializer === node.parent))) {
        const scope = bindingScope(node);
        const entries = initializedBindings.get(scope) || [];
        function names(binding) {
          if (ts.isIdentifier(binding)) entries.push({ name: binding.text, declaration: node });
          else for (const element of binding.elements) if (ts.isBindingElement(element)) names(element.name);
        }
        names(node.name);
        initializedBindings.set(scope, entries);
      }
      if (ts.isBinaryExpression(node) && node.operatorToken.kind >= ts.SyntaxKind.FirstAssignment && node.operatorToken.kind <= ts.SyntaxKind.LastAssignment) recordWrite(node.left);
      if ((ts.isPrefixUnaryExpression(node) || ts.isPostfixUnaryExpression(node)) && (node.operator === ts.SyntaxKind.PlusPlusToken || node.operator === ts.SyntaxKind.MinusMinusToken)) recordWrite(node.operand);
      if (ts.isDeleteExpression(node)) recordWrite(calleeOf(node));
      if ((ts.isForInStatement(node) || ts.isForOfStatement(node)) && !ts.isVariableDeclarationList(node.initializer)) recordWrite(node.initializer);
      if (ts.isWithStatement(node) || (ts.isCallExpression(node) && ts.isIdentifier(ts.skipOuterExpressions(calleeOf(node))) && ts.skipOuterExpressions(calleeOf(node)).text === "eval")) dynamicFiles.add(file.fileName);
      ts.forEachChild(node, scan);
    }
    scan(file);
  }
  function concreteCallable(declaration) {
    if (ts.isFunctionDeclaration(declaration) && declaration.body) return declaration;
    if (ts.isVariableDeclaration(declaration) && declaration.initializer && (declaration.parent.flags & ts.NodeFlags.Const) && (ts.isArrowFunction(declaration.initializer) || ts.isFunctionExpression(declaration.initializer))) return declaration.initializer;
  }
  function enclosingCaller(node) {
    let parent = node.parent;
    while (parent) {
      if (ts.isFunctionLike(parent)) return parent.body && parent.body.pos <= node.pos && node.end <= parent.body.end ? parent : undefined;
      if (ts.isClassDeclaration(parent) || ts.isClassExpression(parent) || ts.isPropertyDeclaration(parent)) return undefined;
      parent = parent.parent;
    }
  }
  function bindingScope(declaration) {
    let parent = declaration.parent;
    while (parent && !ts.isSourceFile(parent) && !ts.isFunctionLike(parent)) parent = parent.parent;
    return parent;
  }
  function calleeOf(node) { return ts.isTaggedTemplateExpression(node) ? node.tag : node.expression; }
  function commonJs(file) {
    return !!file.commonJsModuleIndicator || /\.c(js|ts)$/.test(file.fileName)
      || file.impliedNodeFormat === ts.ModuleKind.CommonJS || options.module === ts.ModuleKind.CommonJS;
  }
  function admission(node, symbol, signature, calleeType) {
    if (ts.isNewExpression(node)) return { reason: "unsupported_constructor" };
    if (!ts.isIdentifier(calleeOf(node))) return { reason: "unsupported_callee" };
    if (calleeType.flags & ts.TypeFlags.Any) return { reason: "any_or_error" };
    if (!signature?.declaration) return { reason: "unknown_signature" };
    const caller = enclosingCaller(node);
    if (!caller) return { reason: "no_body_caller" };
    const bodies = (symbol?.declarations || []).map(concreteCallable).filter(Boolean);
    if (bodies.length !== 1) return { reason: "nonunique_or_nonconcrete_binding" };
    const target = bodies[0], targetFile = target.getSourceFile();
    if (targetFile !== node.getSourceFile() && (commonJs(targetFile) || commonJs(node.getSourceFile()))) return { reason: "unsupported_commonjs_inter_module" };
    if (!inside(targetFile.fileName, root) || targetFile.fileName.includes("/node_modules/") || targetFile.isDeclarationFile) return { reason: "external" };
    const mutations = writes.get(symbol) || [];
    if (mutations.length) return { reason: "binding_write", mutations };
    if (dynamicFiles.has(node.getSourceFile().fileName) || dynamicFiles.has(targetFile.fileName)) return { reason: "dynamic_scope" };
    const declaration = symbol.declarations.find(d => concreteCallable(d) === target);
    let owner = declaration.parent;
    while (owner && !ts.isSourceFile(owner) && !ts.isFunctionLike(owner)) {
      if (ts.isModuleBlock(owner) || ts.isModuleDeclaration(owner)) return { reason: "unsupported_namespace_binding" };
      owner = owner.parent;
    }
    const scope = bindingScope(declaration);
    if (writtenBindings.get(scope)?.has(symbol.name)) return { reason: "binding_scope_write" };
    if ((initializedBindings.get(scope) || []).some(entry => entry.declaration !== declaration && entry.name === symbol.name)) return { reason: "binding_redeclaration" };
    if (ts.isSourceFile(scope) && !ts.isExternalModule(scope) && !scope.commonJsModuleIndicator) return { reason: "unproven_script_global" };
    const overlaps = (d, candidate) => d.path === path.relative(root, candidate.getSourceFile().fileName) && d.start !== undefined && d.start < candidate.end && candidate.pos < d.start + (d.length || 1);
    if (diagnostics.some(d => overlaps(d, node) || overlaps(d, target))) return { reason: "affected_diagnostic" };
    return { reason: "proven_binding_candidate", target: location(target), target_body: location(target.body), caller: location(caller), signature: location(signature.declaration) };
  }
  verifyHost();
  if (program.getOptionsDiagnostics().length || program.getGlobalDiagnostics().length || program.getSyntacticDiagnostics().length || diagnostics.some(d => [2307, 2688, 2792, 6053, 6305].includes(d.code))) throw new Error("invalid semantic program configuration, syntax or module resolution");
  const sites = [];
  const reportFiles = selectedFiles && new Set(selectedFiles);
  function location(node) {
    const file = node.getSourceFile();
    let fileLocation = fileLocations.get(file);
    if (!fileLocation) {
      fileLocation = { path: inside(file.fileName, root) ? path.relative(root, file.fileName) : "@typescript/lib/" + path.relative(tools, file.fileName), ascii: Buffer.byteLength(file.text) === file.text.length };
      fileLocations.set(file, fileLocation);
    }
    const start = node.getStart(file), end = node.getEnd();
    return { path: fileLocation.path, start, end, start_byte: fileLocation.ascii ? file.text.slice(0, start).length : Buffer.byteLength(file.text.slice(0, start)), end_byte: fileLocation.ascii ? file.text.slice(0, end).length : Buffer.byteLength(file.text.slice(0, end)), line: file.getLineAndCharacterOfPosition(start).line + 1, kind: ts.SyntaxKind[node.kind] };
  }
  for (const file of program.getSourceFiles()) {
    if (!inside(file.fileName, root) || file.fileName.includes("/node_modules/") || file.isDeclarationFile || (reportFiles && !reportFiles.has(path.relative(root, file.fileName)))) continue;
    function visit(node) {
      if (ts.isCallExpression(node) || ts.isNewExpression(node) || ts.isTaggedTemplateExpression(node)) {
        let symbol = checker.getSymbolAtLocation(calleeOf(node));
        if (symbol && symbol.flags & ts.SymbolFlags.Alias) symbol = checker.getAliasedSymbol(symbol);
        const signature = checker.getResolvedSignature(node);
        const calleeType = checker.getTypeAtLocation(calleeOf(node));
        const declarations = symbol?.declarations || [];
        let parent = node.parent;
        while (parent && !ts.isFunctionLike(parent)) parent = parent.parent;
        sites.push({ admission: admission(node, symbol, signature, calleeType), location: location(node), callee: calleeOf(node).getText(file), callee_location: location(calleeOf(node)), callee_name: location(ts.isPropertyAccessExpression(calleeOf(node)) ? calleeOf(node).name : calleeOf(node)), callee_any: !!(calleeType.flags & ts.TypeFlags.Any), caller: parent && location(parent), symbol: symbol?.name, declarations: declarations.map(d => {
          const callable = ts.isFunctionLike(d) ? d : ts.isVariableDeclaration(d) && d.initializer && (ts.isArrowFunction(d.initializer) || ts.isFunctionExpression(d.initializer)) ? d.initializer : undefined;
          return { ...location(d), callable: callable && location(callable), body: callable?.body && location(callable.body) };
        }), signature: signature?.declaration && location(signature.declaration) });
      }
      ts.forEachChild(node, visit);
    }
    visit(file);
  }
  const aliases = [];
  for (const file of program.getSourceFiles()) {
    if (!inside(file.fileName, root) || file.fileName.includes("/node_modules/") || file.isDeclarationFile) continue;
    function visitAlias(node) {
      if ((ts.isImportSpecifier(node) || ts.isImportClause(node) || ts.isNamespaceImport(node)) && node.name) {
        const symbol = checker.getSymbolAtLocation(node.name);
        if (symbol?.flags & ts.SymbolFlags.Alias) {
          const target = checker.getAliasedSymbol(symbol);
          aliases.push({ declaration: location(node), targets: (target.declarations || []).map(location) });
        }
      }
      ts.forEachChild(node, visitAlias);
    }
    visitAlias(file);
  }
  return { diagnostics, sites, aliases, program_diagnostics: { options: program.getOptionsDiagnostics().length, global: program.getGlobalDiagnostics().length, syntactic: program.getSyntacticDiagnostics().length } };
}
