function scriptTarget() { return 1; }
globalThis.scriptTarget = () => 2;
function scriptCaller() { return scriptTarget(); } // site:script-global-write
