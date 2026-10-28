namespace N { export function f() { return 1; } }
N["f"] = () => 2;
import alias = N.f;
export function namespaceCaller() { alias(); } // site:namespace-mutation
