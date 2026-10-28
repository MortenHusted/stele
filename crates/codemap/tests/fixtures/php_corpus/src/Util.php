<?php

namespace App\Util;

use Vendor\Pkg\{Alpha, Beta as B};

function slugify(string $s): string
{
    return strtolower($s);
}
