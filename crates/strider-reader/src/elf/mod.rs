pub(crate) mod dynamic;
pub(crate) mod encodings;
mod load;
mod reader;
pub(crate) mod relocations;
pub(crate) mod sections;

#[cfg(test)]
mod encodings_tests;

pub use load::{ElfImage, OwnedElf, RelocationMode, load_elf};
pub use reader::ElfFileMemReader;
pub use relocations::local_entry_offset;
#[cfg(feature = "test-util")]
pub use sections::LoadFilter;
pub use sections::{
    AddressSet, ElfSectionLayout, OpdTable, RegionSource, mapping_symbol_data_ranges,
    mapping_symbol_kind, without_ranges,
};
