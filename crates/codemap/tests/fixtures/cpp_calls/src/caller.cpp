#include "ops.hpp"

namespace ops {

int run(Base* base, Child& child, Grandchild& grandchild, Fn fn) {
  int selected = pick(1);             // site:pick-int
  int scaled = pick(1, 2);            // site:pick-scale
  int own = child.concrete();         // site:child-concrete
  int via_base = base->concrete();    // site:base-concrete
  int shared = Base::shared(7);       // site:base-static
  int virtual_id = base->id();        // site:virtual-base
  int virtual_child = child.id();     // site:virtual-child
  int virtual_grandchild = grandchild.id();  // site:virtual-grandchild
  int pointed = fn(1);                // site:fn-pointer
  int applied = apply(fn, 1);         // site:apply
  int expanded = through_macro(3);    // site:through-macro
  int templated = identity(4);        // site:template-identity
  int inlined = header_pick();        // site:header-target
  int other = decoy::pick(1);         // site:decoy-pick
  return selected + scaled + own + via_base + virtual_id + virtual_child +
         pointed + applied + expanded + templated + inlined + other + shared + virtual_grandchild;
}

}  // namespace ops
