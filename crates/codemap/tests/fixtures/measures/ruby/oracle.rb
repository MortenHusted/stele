def straight
  1
end

def conditionals(a, b)
  if a
    1
  elsif b
    2
  else
    3
  end
end

def unless_form(a)
  unless a
    1
  end
end

def ternary(a)
  a ? 1 : 2
end

def modifiers(a)
  work if a
  work unless a
end

def loops(items)
  while ready
    work
  end
  until done
    work
  end
  for item in items
    work(item)
  end
end

def loop_modifiers
  work while ready
  work until done
end

def when_arms(value)
  case value
  when 1, 2
    :low
  when 3
    :high
  else
    :other
  end
end

def in_arms(value)
  case value
  in [a]
    a
  in {b:}
    b
  else
    nil
  end
end

def rescues
  begin
    work
  rescue ArgumentError, TypeError
    retry_work
  rescue StandardError
    fallback
  ensure
    cleanup
  end
end

def rescue_modifier
  work rescue fallback
end

def booleans(a, b)
  a && b
  a || b
  a and b
  a or b
end

def safe_navigation(value)
  value&.owner&.name
end

def blocks(items)
  items.map do |item|
    work if item && ready
    -> { item ? 1 : 2 }.call
  end.select { |item| item || fallback }
  items&.size
end

def self.endless(value) = value ? 1 : 0

def line_classes
  # ordinary comment

=begin
  if && rescue are only comment text
=end
  value = "# not a comment" # trailing comment

  value
end

def every_parameter(a, b = 1, *rest, c, d:, e: 2, **options, &block)
  [a, b, rest, c, d, e, options, block]
end

def forwarding(...)
  target(...)
end

def no_keywords(a, **nil)
  a
end

def elsif_chain(a)
  if a == 1
    :one
  elsif a == 2
    :two
  elsif a == 3
    :three
  else
    :many
  end
end

def deep(items)
  if items.any?
    items.each do |item|
      if item.ready?
        while item.pending?
          item.step
        end
      end
    end
  end
end

def chains(items)
  items.map(&:name).compact.first.to_s
  items.select { |item| item.ready? }.map(&:name)
  Widget.new(items).build
end

def assigns
  @first = 1
  @second ||= 2
  @first += 1
  @third, @fourth = 3, 4
  @first
end

def reads_only
  @first + @second
end
