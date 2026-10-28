function tag(parts: TemplateStringsArray) { return parts[0]; }
export function taggedCaller() { return tag`literal`; } // site:tagged-template
class Box { constructor() {} }
export function constructorCaller() { return new Box(); } // site:constructor
