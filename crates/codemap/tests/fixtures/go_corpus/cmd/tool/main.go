package main

import (
	. "fmt"
	_ "embed"

	y "github.com/x/y"
)

func main() {
	run()
	Println(y.Vendored)
}

func run() {}
