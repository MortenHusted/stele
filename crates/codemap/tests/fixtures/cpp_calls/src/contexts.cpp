#include "ops.hpp"
#include "typeinfo"

void context_calls() {
  struct Local { int value = ops::pick(1); }; // site:local-initializer
  (void)sizeof(ops::pick(1)); // site:unevaluated-sizeof
  using Result = decltype(ops::pick(1)); // site:unevaluated-decltype
  bool safe = noexcept(ops::pick(1)); // site:unevaluated-noexcept
  bool available = requires { ops::pick(1); }; // site:unevaluated-requires
  (void)typeid(ops::pick(1)); // site:unevaluated-typeid
}
