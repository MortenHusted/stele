// Corpus fixture: macro definitions.

macro_rules! say_hello {
    () => {
        println!("hello")
    };
}

macro_rules! duplicate {
    ($e:expr) => {
        $e
    };
}
