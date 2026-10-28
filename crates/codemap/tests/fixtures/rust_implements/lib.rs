#![allow(dead_code)]
mod decoy {
    pub trait Write {}
    pub struct External;
}
mod actual {
    use std::io::{Empty as External, Write};
    pub struct Sink;
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> { Ok(bytes.len()) }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    pub trait LocalTrait {}
    impl LocalTrait for External {}
}
mod positive {
    pub trait Marker {}
    pub struct Item;
    impl Marker for Item {}
}
impl crate::decoy::Write for crate::positive::Item {}
mod aliased {
    use crate::positive::Marker as Tag;
    pub struct Thing;
    impl Tag for Thing {}
}
mod shapes_a { pub trait Shape {} }
mod shapes_b {
    pub trait Shape {}
    pub struct Square;
    impl Shape for Square {}
}
mod globbed {
    use crate::decoy::*;
    pub struct Z;
    impl Write for Z {}
}
