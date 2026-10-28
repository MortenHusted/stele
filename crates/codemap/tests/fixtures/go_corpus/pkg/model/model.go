package model

type ID int

type Entity struct{ ID ID }

type Alias = Entity

func (e Entity) Key() string { return "" }

type List[T any] struct{ items []T }

func (l *List[T]) Push(t T) { l.items = append(l.items, t) }

func Map[T, U any](xs []T, f func(T) U) []U {
	out := make([]U, 0, len(xs))
	for _, x := range xs {
		out = append(out, f(x))
	}
	return out
}
