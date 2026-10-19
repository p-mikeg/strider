pub(crate) mod encodings;
pub mod load;
pub mod reader;
pub(crate) mod relocations;
pub(crate) mod sections;

#[cfg(test)]
mod encodings_tests;

pub use load::{ElfImage, OwnedElf, RelocationMode, load_elf};
pub use reader::ElfFileMemReader;
#[cfg(feature = "test-util")]
pub use sections::LoadFilter;
pub use sections::{
    AddressSet, ElfSectionLayout, OpdTable, RegionSource, mapping_symbol_data_ranges,
    without_ranges,
};
