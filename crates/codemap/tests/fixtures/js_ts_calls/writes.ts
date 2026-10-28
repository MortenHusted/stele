export function assigned() { return 1; }
({ value: assigned } = { value: () => 2 });
export function propertyPatternCaller() { return assigned(); } // site:destructured-property-write
export function shorthand() { return 3; }
({ shorthand } = { shorthand: () => 4 });
export function shorthandCaller() { return shorthand(); } // site:destructured-shorthand-write
export function iterated() { return 5; }
for (iterated of [() => 6]) {}
export function iterationCaller() { return iterated(); } // site:iteration-write
export function incremented() { return 7; }
incremented++;
export function incrementCaller() { return incremented(); } // site:update-write
