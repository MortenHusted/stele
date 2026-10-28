// Clone oracle, file one. summarise is repeated verbatim in b.go (an exact
// clone); tally is repeated with every name and literal changed (a renamed
// clone); nearA shares under fifty tokens with its sibling; wide is
// repeated verbatim but covers under five lines.
package clones

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

func otherA(flag bool) int {
	if flag {
		return 1
	}
	return 2
}

func wide() int {
	v := []int{1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30}
	return len(v)
}

func tally(items []uint32, limit uint32) (uint32, uint32) {
	index := 0
	var kept uint32
	var dropped uint32
	for index < len(items) {
		item := items[index]
		if item <= limit {
			kept += item
		} else {
			dropped += 1
		}
		index += 1
	}
	return kept, dropped
}

func nearA(rows []string) int {
	seen := 0
	for _, row := range rows {
		if row == "" {
			continue
		}
		seen += 1
	}
	return seen
}
