# Twin oracle, file two. Every twin sits between neighbours that differ
# from its partner's, so no clone run reaches past a body.

def lead
  1
end

def exact_b(items, limit)
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

MARK = 7

def near_b(values)
  out = {}
  total = 0
  largest = 0
  return out if values.empty?
  values.each do |value|
    total += value
    if value > largest
      largest = value
    end
  end
  out["total"] = total
  out["largest"] = largest
  total = total.abs
  out["count"] = values.length
  out["mean"] = values.empty? ? 0 : total / values.length
  out["gap"] = largest - total
  out["kind"] = :summary
  out.freeze
end

Marker = Struct.new(:tag)

def prefix_b(rows)
  seen = 0
  skipped = 0
  case rows.length
  when 0
    skipped = 1
  when 1
    seen = rows.first.length
  when 2..10
    seen = rows.length
    skipped = rows.count(&:empty?)
  else
    seen = rows.length * 2
    skipped = rows.length - 10
  end
  weighted = seen * 3 + skipped * 2
  weighted.clamp(seen, 1000)
end

def small_b(x)
  x > 1 ? x * 2 : 1
end
