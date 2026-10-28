# Clone oracle, file two. Every method that has a twin in a.rb sits between
# different neighbours here, so a token run never reaches past the method
# itself.

def lead
  1
end

def summarise(values)
  out = {}
  total = 0
  largest = 0
  # The running total and the largest value seen so far.
  values.each do |value|
    total += value
    if value > largest
      largest = value
    end
  end

  out["total"] = total
  out["largest"] = largest
  out["count"] = values.length
  out
end

Marker = Struct.new(:tag)

def wide
  v = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30]
  v.sum
end

LIMIT = 3

def count_up(numbers, ceiling)
  raise ArgumentError, "ceiling" if ceiling < 0
  cursor = 0
  sum = 0
  skipped = 0
  while cursor < numbers.length
    number = numbers[cursor]
    if number <= ceiling
      sum += number
    else
      skipped += 7
    end
    cursor += 2
  end
  [sum, skipped]
end

def near_tokens_b(rows, marker = nil)
  seen = 0
  rows.each do |row|
    if row.start_with?("#")
      seen += 2
    else
      seen += 1
    end
  end
  seen
end

require "set"
