pub mod blob;
pub mod bundle;

pub(crate) mod constants {
    // These are copied over from alloy so that we don't make fuel-core pull in alloy just for the
    // constants.
    pub const BYTES_PER_BLOB: usize = 131_072;
    pub const FIELD_ELEMENTS_PER_BLOB: usize = 4096;
    pub const USABLE_BITS_PER_FIELD_ELEMENT: usize = 254;
}
