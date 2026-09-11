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
//! The hand-written stubs under `strider/` are what ship with the wheel and
//! what `uv run pyright` gates; the generated `.pyi` is a reference to read
//! them against, consumed by nothing.
//!
//! An `[example]`, so it builds only on demand and leaves the default
//! `cargo build` / `cargo test` flow the snapshot baselines depend on alone.

use std::fs;
use std::path::PathBuf;

use pyo3_stub_gen::Result;

/// Sibling of the hand-written stubs, so re-running `stub_gen` can never
/// overwrite what ships with the wheel.
const OUT_DIR: &str = "strider/_generated";

fn main() -> Result<()> {
    // `StubInfo`'s own writer emits straight to `strider/*.pyi`, clobbering
    // the hand-written stubs, so walk its `modules` map and write to a
    // sibling directory instead.
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
