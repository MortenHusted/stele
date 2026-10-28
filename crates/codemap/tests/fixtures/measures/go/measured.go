// Package oracle is the Go measures oracle: every count in expected.json
// was written by hand before the counter ran, with the rule behind each
// increment.
package oracle

import "fmt"

func straight(a, b int) int {
	return a + b
}

func nestedIfs(x int) string {
	if x > 10 {
		if x > 100 {
			return "huge"
		}
		return "big"
	} else if x > 5 {
		return "medium"
	}
	return "small"
}

func logic(a, b, c bool) bool {
	return (a && b) || (!c && a)
}

func loops(items []int) int {
	total := 0
	for _, item := range items {
		total += item
	}
	for i := 0; i < 3; i++ {
		total++
	}
	for total > 100 {
		total -= 10
	}
	return total
}

func switching(kind string) int {
	switch kind {
	case "a":
		return 1
	case "b":
		return 2
	case "c":
		return 3
	default:
		return 0
	}
}

func typeSwitching(v interface{}) string {
	switch v.(type) {
	case int:
		return "int"
	case string:
		return "string"
	default:
		return "other"
	}
}

func selecting(a, b <-chan int) int {
	select {
	case x := <-a:
		return x
	case y := <-b:
		return y
	}
}

func withClosure(rows []int) []int {
	keep := make([]int, 0)
	filter := func(r int) bool {
		if r < 0 {
			return false
		}
		return r > 0 && r < 10
	}
	for _, r := range rows {
		if filter(r) {
			keep = append(keep, r)
		}
	}
	return keep
}

func commented() int {
	// one
	// two

	return 1
}

func printing() {
	fmt.Println("x")
}

// Counter is a receiver for the method cases.
type Counter struct{ n int }

func (c *Counter) Bump(by int) int {
	if by > 0 {
		c.n += by
	}
	return c.n
}

func (c Counter) Value() int {
	return c.n
}

// Shape carries two interface methods, which have no body.
type Shape interface {
	Area() float64
	Name() string
}

func variadic(base int, xs ...int) int {
	total := base
	for _, x := range xs {
		total += x
	}
	return total
}

func elseChain(a int) int {
	if a == 1 {
		return 1
	} else if a == 2 {
		return 2
	} else if a == 3 {
		return 3
	}
	return 0
}

func deep(rows [][]int) int {
	total := 0
	for _, row := range rows {
		if len(row) > 0 {
			count := func(v int) int {
				if v > 0 {
					return 1
				}
				return 0
			}
			total += count(row[0])
		}
	}
	return total
}

// Repo is a receiver for the chain cases.
type Repo struct{ items []int }

// Find returns the repo itself.
func (r *Repo) Find(id int) *Repo {
	return r
}

// Items returns the repo's items.
func (r *Repo) Items() []int {
	return r.items
}

// Service holds a repo.
type Service struct{ repo *Repo }

func (s *Service) chains(id int, m map[string]*Repo, v interface{}) int {
	a := s.repo.Find(id).Items()
	b := m["k"].items
	c := v.(*Repo).Items()
	d := (s.repo).items
	return len(a) + len(b) + len(c) + len(d)
}
