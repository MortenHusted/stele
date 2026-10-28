pub struct Worker;

impl Worker {
    pub fn receiver(&self, value: u32) -> u32 {
        value
    }

    pub fn associated(value: u32) -> u32 {
        value
    }
}

pub fn cross_crate_free(value: u32) -> u32 {
    value
}

// A function that calls itself is a verified call like any other; the edge
// from a definition to itself must not be dropped as noise.
pub fn descend(depth: u32) -> u32 {
    if depth == 0 { 0 } else { descend(depth - 1) }
}

// Called only inside a macro argument: the grammar sees tokens there, not a
// call expression, so the collector must find it by shape.
pub fn in_macro_arg(value: u32) -> u32 {
    value
}
