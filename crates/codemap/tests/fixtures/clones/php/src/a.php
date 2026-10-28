<?php

// Clone oracle, file one. `summarise` is repeated verbatim in b.php (an
// exact clone); `tally` is repeated with every name and literal changed
// (a renamed clone); `nearA` shares under fifty tokens with its sibling;
// `wide` is repeated verbatim but covers under five lines.

namespace Oracle;

function summarise(array $values): array
{
    $out = [];
    $total = 0;
    $largest = PHP_INT_MIN;
    // The running total and the largest value seen so far.
    foreach ($values as $value) {
        $total += $value;
        if ($value > $largest) {
            $largest = $value;
        }
    }

    $out['total'] = $total;
    $out['largest'] = $largest;
    $out['count'] = count($values);
    return $out;
}

function otherA(bool $flag): int
{
    return $flag ? 1 : 2;
}

function wide(): int {
    $v = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30];
    return array_sum($v);
}

function tally(array $items, int $limit): array
{
    $index = 0;
    $kept = 0;
    $dropped = 0;
    while ($index < count($items)) {
        $item = $items[$index];
        if ($item <= $limit) {
            $kept += $item;
        } else {
            $dropped += 1;
        }
        $index += 1;
    }
    return [$kept, $dropped];
}

function nearA(array $rows): int
{
    $seen = 0;
    foreach ($rows as $row) {
        if ($row === '') {
            continue;
        }
        $seen += 1;
    }
    return $seen;
}
