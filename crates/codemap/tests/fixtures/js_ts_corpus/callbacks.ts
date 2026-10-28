export function register() { const label = "🦄"; consume(() => 1); }
function consume(callback: () => number) { return callback(); }
