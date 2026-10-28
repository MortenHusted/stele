// Malformed fixture: the struct body is never closed.
// Extraction must report `parse_errors > 0` and still succeed.

struct Missing {
