export function reassigned() { return 1; }
export function replace() { reassigned = () => 2; }
export function afterWrite() { return reassigned(); } // site:reassigned-function
let mutable = () => 3;
export function mutableCall() { return mutable(); } // site:mutable-binding
export function callbackCall(callback: () => number) { return callback(); } // site:parameter
export function unknownCall(callee: any) { return callee(); } // site:any
export function shadow(local: () => number) { return local(); } // site:shadowed-name
export function evaluate() {
  function victim() { return 4; }
  eval("victim = () => 5"); // site:eval
  return victim(); // site:eval-affected
}
const receiver = { method() { return 6; } };
export function methodCall() { return receiver.method(); } // site:receiver
export function computedCall(key: "method") { return receiver[key](); } // site:computed
export function broken() { return absent(); } // site:error-recovery
