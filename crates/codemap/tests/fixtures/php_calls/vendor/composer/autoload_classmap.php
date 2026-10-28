<?php

// A trap, not a class map. phpactor's composer extension includes this file
// from the analyzed tree when its autoloader is enabled; codemap launches
// phpactor with that extension disabled, and the semantic oracle asserts the
// marker below never appears.
file_put_contents(__DIR__ . '/../../.codemap-autoloader-executed', "classmap\n");

return [];
