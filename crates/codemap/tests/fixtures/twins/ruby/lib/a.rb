# Twin oracle, file one. `exact_a` has a verbatim twin in b.rb (a clone
# group and a twin at once); `near_a` has a twin in b.rb that inserts two
# statements, so no fifty-token run survives but most five-token shingles
# do; `prefix_a` shares only its opening tokens with `prefix_b`; `small_a`
# and `small_b` are identical but under eighty tokens.

def exact_a(items, limit)
  raise ArgumentError, "limit" if limit < 0
  index = 0
  kept = 0
  dropped = 0
  skipped = []
  while index < items.length
    item = items[index]
    if item <= limit
      kept += item
    elsif item > limit * 2
      skipped << item
    else
      dropped += 1
    end
    index += 1
  end
  kept = kept.clamp(0, limit * 100)
  total = kept + dropped + skipped.length
  [kept, dropped, total]
end

def near_a(values)
  out = {}
  total = 0
  largest = 0
  values.each do |value|
    total += value
    if value > largest
      largest = value
    end
  end
  out["total"] = total
  out["largest"] = largest
  out["count"] = values.length
  out["mean"] = values.empty? ? 0 : total / values.length
  out["gap"] = largest - total
  out["kind"] = :summary
  out.freeze
end

def prefix_a(rows)
  seen = 0
  skipped = 0
  rows.each do |row|
    if row.empty?
      skipped += 1
      next
    end
    if row.start_with?("#")
      skipped += 2
    elsif row.end_with?(";")
      seen += 3
    else
      seen += 1
    end
  end
  bonus = seen / 2
  seen + skipped + bonus
end

def small_a(x)
  x > 1 ? x * 2 : 1
end
