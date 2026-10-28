export function redeclarationCaller() {
  function f() { return 1; }
  var f = () => 2;
  return f(); // site:redeclaration-initializer
}
export function forVarCaller() {
  function f() { return 1; }
  for (var f of [() => 2]) {}
  return f(); // site:for-var-redeclaration
}
export function forDestructuredVarCaller() {
  function f() { return 1; }
  for (var { f } of [{ f: () => 2 }]) {}
  return f(); // site:for-destructured-var-redeclaration
}
export function varAssignmentCaller() {
  function f() { return 1; }
  var f;
  f = () => 2;
  return f(); // site:var-redeclaration-assignment
}
