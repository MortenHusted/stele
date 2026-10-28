<?php

// PHP measures oracle: every count in expected.json was written by hand
// before the counter ran, with the rule behind each increment.

namespace Oracle;

function straight(int $a, int $b): int
{
    return $a + $b;
}

function nestedIfs(int $x): string
{
    if ($x > 10) {
        if ($x > 100) {
            return 'huge';
        }
        return 'big';
    } elseif ($x > 5) {
        return 'medium';
    } else {
        return 'small';
    }
}

function logic(bool $a, bool $b, bool $c): bool
{
    $r = ($a && $b) || $c;
    $s = $a and $b;
    $t = $a or $c;
    $u = $b xor $c;
    return $r;
}

function coalesce(?string $name, array $opts): string
{
    $label = $name ?? 'anon';
    $mode = $opts['mode'] ?? 'plain';
    return $mode === 'loud' ? strtoupper($label) : $label;
}

function loops(array $items): int
{
    $total = 0;
    foreach ($items as $item) {
        $total += $item;
    }
    for ($i = 0; $i < 3; $i++) {
        $total++;
    }
    while ($total > 100) {
        $total -= 10;
    }
    do {
        $total++;
    } while ($total < 5);
    return $total;
}

function matching(int $code): string
{
    return match (true) {
        $code === 200 => 'ok',
        $code === 404 => 'missing',
        default => 'other',
    };
}

function switching(string $kind): int
{
    switch ($kind) {
        case 'a':
            return 1;
        case 'b':
            return 2;
        default:
            return 0;
    }
}

function guarded(callable $fn): mixed
{
    try {
        return $fn();
    } catch (\RuntimeException $e) {
        return null;
    } catch (\Throwable $e) {
        return false;
    }
}

function withClosure(array $rows): array
{
    $keep = array_filter($rows, function ($row) {
        if ($row === null) {
            return false;
        }
        return $row > 0 && $row < 10;
    });
    return array_map(fn ($r) => $r > 5 ? $r * 2 : $r, $keep);
}

function commented(): int
{
    // one
    // two

    return 1;
}

class Counter
{
    private int $n = 0;

    public function bump(int $by): int
    {
        if ($by > 0) {
            $this->n += $by;
        }
        return $this->n;
    }

    public static function make(): self
    {
        /* a block comment on its own line */
        return new self();
    }
}

class Promoted
{
    public function __construct(private int $a, public readonly string $b = 'x', int ...$rest)
    {
        $this->a = $a;
    }
}

class Chains
{
    private Repo $repo;

    public function deep(array $rows): int
    {
        $total = 0;
        foreach ($rows as $row) {
            if ($row !== null) {
                $total += count(array_filter($row, function ($v) {
                    if ($v > 0) {
                        return true;
                    }
                    return false;
                }));
            }
        }
        return $total;
    }

    public function chains(int $id, ?Row $row, array $items, string $s): int
    {
        $a = $this->repo->find($id)->items();
        $b = $row?->owner?->name;
        $c = Str::of($s)->upper();
        $d = $items[0]->name;
        return strlen($s) + count($a) + strlen($b) + strlen($c) + strlen($d);
    }
}
