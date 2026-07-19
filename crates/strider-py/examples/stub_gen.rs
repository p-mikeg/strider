//! `.pyi` generator for the macro-emitted reference pattern types.
//!
//! ```bash
//! cargo run -p strider-py --example stub_gen \
//!     --features stub_gen --no-default-features
//! ```
//!
//! `--no-default-features` turns off pyo3's `extension-module` so the
//! binary actually links libpython at load time; `--features stub_gen`
//! swaps in `pyo3/auto-initialize` so `Python::with_gil(...)` works here.
//!
//! Output lands in the gitignored `crates/strider-py/strider/_generated/`.
//! The hand-written `strider/pattern.pyi` is what ships with the wheel;
//! the generated `.pyi` is only the test oracle consumed by
//! `tests/python/test_reference_pyi.py` (mypy --strict).
//!
//! This is an `[example]`, not a `[bin]`: examples build only on demand,
//! so the target doesn't change the default `cargo build` / `cargo test`
//! flow the snapshot baselines depend on. A `[bin]` would pull in
//! unconditional dependency reachability, dead-code lints, and binary
//! linkage on `cargo build --workspace`.

use std::fs;
use std::path::PathBuf;

use pyo3_stub_gen::Result;

/// Sibling of the hand-written `pattern.pyi`, so re-running `stub_gen`
/// can never overwrite the stubs that ship with the wheel.
const OUT_DIR: &str = "strider/_generated";

fn main() -> Result<()> {
    // `StubInfo`'s own writer emits straight to `strider/*.pyi`, clobbering
    // the hand-written stubs, so we walk its `modules` map and write to a
    // sibling directory ourselves.
    let stub = strider_py::stub_info()?;

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out_root = manifest_dir.join(OUT_DIR);
    fs::create_dir_all(&out_root)?;

    for (mod_name, module) in &stub.modules {
        let rel = mod_name.replace('.', "/");
        let dest = out_root.join(format!("{rel}.pyi"));
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&dest, format!("{module}"))?;
        println!("wrote {}", dest.display());
    }
    println!("Reference stubs generated under {}", out_root.display(),);
    Ok(())
}
