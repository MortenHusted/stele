function leaf() { return 1; }
export function defaultCaller(value = leaf()) { return value; } // site:default-argument
export function classOwner() {
  class Inner { field = leaf(); } // site:class-field
  return Inner;
}
