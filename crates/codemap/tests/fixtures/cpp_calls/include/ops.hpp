#pragma once

namespace ops {

int pick(int value);
int pick(int value, int scale);

struct Base {
  virtual int id() const;
  int concrete() const;
  static int shared(int value);
};

struct Child : Base {
  int id() const override;
  int concrete() const;
};

struct Grandchild : Child {
  int id() const;
};

using Fn = int (*)(int);

int apply(Fn fn, int value);
int through_macro(int value);

template <typename T>
T identity(T value);

inline int header_pick() {  // def:header-pick
  return pick(1);           // site:header-inner
}

}  // namespace ops

namespace decoy {

int pick(int value);

}  // namespace decoy
