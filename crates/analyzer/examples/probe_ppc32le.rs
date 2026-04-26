//! Try to lift a single `add r3,r3,r4` instruction with PPC_32_LE Sleigh.
use object::{Object, ObjectSymbol};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // First: try via the actual ELF reader path.
    let path = "fixtures/out/ppc32le/arithmetic.elf";
    let obj = reader::load_elf(path)?;
    let mem = reader::ElfFileMemReader::from_object(&obj)?;
    let mut elf_sleigh = rsleigh::Sleigh::new(
        rsleigh::sla_spec::SLA_SPEC_PPC_32_LE,
        rsleigh::pspec::PSPEC_PPC_32,
        mem,
    )?;
    let add_addr = obj.symbol_by_name("add").unwrap().address();
    println!("add symbol addr: 0x{add_addr:x}");
    // Read 4 bytes at that address via the reader directly to see what we get.
    let mem2 = reader::ElfFileMemReader::from_object(&obj)?;
    let mut buf = [0u8; 4];
    let n = <reader::ElfFileMemReader as rsleigh::MemReader>::read(
        &mem2,
        rsleigh::VnAddr { off: add_addr, space: rsleigh::VnSpace::RAM },
        &mut buf,
    )?;
    println!("bytes at add ({n} read): {:02x?}", &buf[..]);
    match elf_sleigh.lift_one(add_addr) {
        Ok(lift) => println!("via ELF reader OK: {} ops", lift.insns.len()),
        Err(e) => println!("via ELF reader ERR: {e:?}"),
    }


    // `add r3,r3,r4` LE bytes (as in the ELF): 14 22 63 7c
    let bytes = vec![0x14, 0x22, 0x63, 0x7c];
    let reader = rsleigh::mem_readers::BufMemReader::new(bytes, 0);
    let mut sleigh = rsleigh::Sleigh::new(
        rsleigh::sla_spec::SLA_SPEC_PPC_32_LE,
        rsleigh::pspec::PSPEC_PPC_32,
        reader,
    )?;
    match sleigh.lift_one(0) {
        Ok(lift) => println!("OK: {} ops", lift.insns.len()),
        Err(e) => println!("ERR: {e:?}"),
    }
    // Try with bytes flipped (BE order):
    let bytes_be = vec![0x7c, 0x63, 0x22, 0x14];
    let reader2 = rsleigh::mem_readers::BufMemReader::new(bytes_be, 0);
    let mut sleigh2 = rsleigh::Sleigh::new(
        rsleigh::sla_spec::SLA_SPEC_PPC_32_LE,
        rsleigh::pspec::PSPEC_PPC_32,
        reader2,
    )?;
    match sleigh2.lift_one(0) {
        Ok(lift) => println!("BE-bytes OK: {} ops", lift.insns.len()),
        Err(e) => println!("BE-bytes ERR: {e:?}"),
    }
    Ok(())
}
