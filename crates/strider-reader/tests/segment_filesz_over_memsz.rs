//! `p_filesz > p_memsz`: the FILE holds more bytes than the image MAPS.
//!
//! No toolchain emits it, but `object` parses it and strider's input domain is
//! untrusted binaries. The mapped extent came from `p_filesz` while the
//! writable/executable bookkeeping came from `p_memsz`, so the tail past
//! `p_memsz` was served as if mapped, and, for a writable-and-executable
//! PT_LOAD, served through a `ReadOnlyMemory` view whose whole contract is
//! that nothing there can change under the analysis.

use strider_reader::{ElfFileMemReader, ReadOnlyMemory};

const BASE: u64 = 0x2000;
/// Bytes the image maps.
const MEMSZ: u64 = 0x10;
/// Bytes the file holds, deliberately larger.
const FILESZ: u64 = 0x20;

/// ELF64-LE x86-64, program headers only, one PT_LOAD with the given flags.
fn elf_with_one_pt_load(p_flags: u32) -> Vec<u8> {
    const EHDR: usize = 64;
    const PHDR: usize = 56;
    let payload_off = (EHDR + PHDR) as u64;
    let mut out = vec![0u8; EHDR + PHDR];

    out[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
    out[4] = 2; // ELFCLASS64
    out[5] = 1; // ELFDATA2LSB
    out[6] = 1; // EV_CURRENT
    out[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
    out[18..20].copy_from_slice(&62u16.to_le_bytes()); // EM_X86_64
    out[20..24].copy_from_slice(&1u32.to_le_bytes()); // e_version
    out[24..32].copy_from_slice(&BASE.to_le_bytes()); // e_entry
    out[32..40].copy_from_slice(&(EHDR as u64).to_le_bytes()); // e_phoff
    out[52..54].copy_from_slice(&(EHDR as u16).to_le_bytes()); // e_ehsize
    out[54..56].copy_from_slice(&(PHDR as u16).to_le_bytes()); // e_phentsize
    out[56..58].copy_from_slice(&1u16.to_le_bytes()); // e_phnum

    let p = EHDR;
    out[p..p + 4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
    out[p + 4..p + 8].copy_from_slice(&p_flags.to_le_bytes());
    out[p + 8..p + 16].copy_from_slice(&payload_off.to_le_bytes()); // p_offset
    out[p + 16..p + 24].copy_from_slice(&BASE.to_le_bytes()); // p_vaddr
    out[p + 24..p + 32].copy_from_slice(&BASE.to_le_bytes()); // p_paddr
    out[p + 32..p + 40].copy_from_slice(&FILESZ.to_le_bytes()); // p_filesz
    out[p + 40..p + 48].copy_from_slice(&MEMSZ.to_le_bytes()); // p_memsz  <-- smaller
    out[p + 48..p + 56].copy_from_slice(&1u64.to_le_bytes()); // p_align

    out.extend(std::iter::repeat_n(0x5au8, FILESZ as usize));
    out
}

/// A read-only PT_LOAD: the bytes past `p_memsz` belong to no mapping, so a
/// fetch there must not be answered.
#[test]
fn bytes_past_p_memsz_are_not_mapped() {
    // PF_R | PF_X
    let image = elf_with_one_pt_load(0b101);
    let obj = object::File::parse(&image[..]).expect("parse");
    let reader = ElfFileMemReader::from_object(&obj).expect("from_object");

    let mut buf = [0u8; 8];
    reader
        .read(BASE, &mut buf)
        .expect("the mapped prefix is readable");

    let past = BASE + MEMSZ;
    assert!(
        reader.read(past, &mut buf).is_err(),
        "{past:#x} is past p_memsz, so no mapping covers it; serving the \
         file's extra bytes there invents memory the image does not have",
    );
}

/// The same shape on a WRITABLE-and-executable PT_LOAD. `ElfFileMemReader`'s
/// ROM view is the fetch set minus the writable ranges, and the writable
/// ranges stop at `p_memsz`, so the tail used to satisfy a read through a
/// trait that promises the bytes cannot change.
#[test]
fn an_rwx_tail_is_not_served_as_read_only_memory() {
    // PF_R | PF_W | PF_X
    let image = elf_with_one_pt_load(0b111);
    let obj = object::File::parse(&image[..]).expect("parse");
    let reader = ElfFileMemReader::from_object(&obj).expect("from_object");

    let mut buf = [0u8; 8];
    assert!(
        reader.read(BASE, &mut buf).is_err(),
        "an RWX mapping is not read-only memory, so even its prefix must be \
         refused here",
    );
    let past = BASE + MEMSZ;
    assert!(
        reader.read(past, &mut buf).is_err(),
        "{past:#x} is the tail of an RWX mapping past p_memsz; answering it \
         lets a constant-address load fold to the file's initial value while \
         a store to the same address is live",
    );
}
