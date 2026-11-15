//! Write the statistics fixture as seatswap's own files, for the bench's fake
//! data folder: `export_tiny <empty folder>`.

use std::path::PathBuf;

use seatswap_core::stats::fixture;

fn main() -> Result<(), String> {
    let mut arguments = std::env::args_os().skip(1);
    let Some(folder) = arguments.next() else {
        return Err("usage: export_tiny <empty folder>".to_string());
    };
    if arguments.next().is_some() {
        return Err("usage: export_tiny <empty folder>".to_string());
    }
    let folder = PathBuf::from(folder);
    fixture::write(&folder)?;
    println!("{}", folder.display());
    Ok(())
}
