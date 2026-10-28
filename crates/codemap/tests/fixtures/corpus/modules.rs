// Corpus fixture: modules and nested modules.

mod outer {
    mod inner {
        pub fn inner_fn() {}
    }

    pub fn outer_fn() {}
}
