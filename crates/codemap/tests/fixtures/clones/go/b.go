// Clone oracle, file two. Every item that has a twin in a.go sits between
// different neighbours here, so a token run never reaches past the item
// itself.
package clones

func lead() int {
	return 1
}

func summarise(values []int64) map[string]int64 {
	out := map[string]int64{}
	var total int64
	largest := int64(-1 << 62)
	// The running total and the largest value seen so far.
	for _, value := range values {
		total += value
		if value > largest {
			largest = value
		}
	}

	out["total"] = total
	out["largest"] = largest
	out["count"] = int64(len(values))
	return out
}

type Marker struct{}

const marker = 1

func wide() int {
	v := []int{1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30}
	return len(v)
}

const limit = 3

func countUp(numbers []uint32, ceiling uint32) (uint32, uint32) {
	cursor := 0
	var sum uint32
	var skipped uint32
	for cursor < len(numbers) {
		number := numbers[cursor]
		if number <= ceiling {
			sum += number
		} else {
			skipped += 7
		}
		cursor += 2
	}
	return sum, skipped
}

var sink = 0

func nearB(rows []string) int {
	seen := 0
	for _, row := range rows {
		if len(row) > 0 && row[0] == '#' {
			seen += 2
		} else {
			seen += 1
		}
	}
	return seen
}
