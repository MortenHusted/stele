# Clone oracle, file one. `summarise` is repeated verbatim in b.rb (an
# exact clone); `tally` is repeated with every name and literal changed
# (a renamed clone); `near_tokens_a` shares under fifty tokens with its
# sibling; `wide` is repeated verbatim but covers under five lines.

require "set"

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

def other_a(flag)
  flag ? 1 : 2
end

def wide
  v = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30]
  v.sum
end

def tally(items, limit)
  raise ArgumentError, "limit" if limit < 0
  index = 0
  kept = 0
  dropped = 0
  while index < items.length
    item = items[index]
    if item <= limit
      kept += item
    else
      dropped += 1
    end
    index += 1
  end
  [kept, dropped]
end

NEAR = 1

def near_tokens_a(rows)
  seen = 0
  rows.each do |row|
    next if row.empty?
    seen += 1
  end
  seen
end
