<?php

// Clone oracle, file two. Every item that has a twin in a.php sits between
// different neighbours here, so a token run never reaches past the item
// itself.

function lead(): int
{
    return 1;
}

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

const MARKER = 1;

function wide(): int {
    $v = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30];
    return array_sum($v);
}

const LIMIT = 3;

function countUp(array $numbers, int $ceiling): array
{
    $cursor = 0;
    $sum = 0;
    $skipped = 0;
    while ($cursor < count($numbers)) {
        $number = $numbers[$cursor];
        if ($number <= $ceiling) {
            $sum += $number;
        } else {
            $skipped += 7;
        }
        $cursor += 2;
    }
    return [$sum, $skipped];
}

final class Marker
{
}

function nearB(array $rows): int
{
    $seen = 0;
    foreach ($rows as $row) {
        if (str_starts_with($row, '#')) {
            $seen += 2;
        } else {
            $seen += 1;
        }
    }
    return $seen;
}
