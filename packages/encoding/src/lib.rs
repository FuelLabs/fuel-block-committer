pub mod blob;
pub mod bundle;

pub(crate) mod constants {
    pub const BYTES_PER_BLOB: usize = 131_072;
    pub const FIELD_ELEMENTS_PER_BLOB: usize = 4096;
    pub const USABLE_BITS_PER_FIELD_ELEMENT: usize = 254;
}
