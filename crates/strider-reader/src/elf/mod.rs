pub(crate) mod encodings;
pub mod load;
pub mod reader;
pub(crate) mod relocations;
pub mod sections;

#[cfg(test)]
mod encodings_tests;

pub use load::{OwnedElf, RelocationMode, load_elf};
pub use reader::ElfFileMemReader;
pub use sections::{
    AddressSet, ElfSectionLayout, LoadFilter, OpdTable, RegionSource, mapping_symbol_data_ranges,
    without_ranges,
};
