package main

import (
	"os"

	"example.invalid/corpus/internal/svc"
)

func main() {
	s := svc.New("corpus")
	if err := svc.Drive(s); err != nil {
		os.Exit(1)
	}
}
