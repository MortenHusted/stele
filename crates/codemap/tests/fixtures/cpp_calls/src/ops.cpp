#include "ops.hpp"

namespace ops {

int pick(int value) { return value; }  // def:pick-int

int pick(int value, int scale) { return value * scale; }  // def:pick-scale

int Base::id() const { return 1; }  // def:base-id

int Base::concrete() const { return 2; }  // def:base-concrete

int Base::shared(int value) { return value; }  // def:base-static

int Child::id() const { return 3; }  // def:child-id

int Child::concrete() const { return 4; }  // def:child-concrete

int Grandchild::id() const { return 5; }  // def:grandchild-id

int apply(Fn fn, int value) {  // def:apply
  return fn(value);            // site:fn-pointer-apply
}

#define DOUBLE(x) pick(x, 2)

int through_macro(int value) {  // def:through-macro
  return DOUBLE(value);         // site:macro-expand
}

template <typename T>
T identity(T value) {  // def:identity
  return value;
}

template int identity<int>(int);

}  // namespace ops

namespace decoy {

int pick(int value) { return value + 1; }  // def:decoy-pick

}  // namespace decoy
