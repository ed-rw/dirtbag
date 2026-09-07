//! The `version` command.

use anyhow::Result;

use crate::tart::Tart;

/// Print the dirtbag version, and the version and path of the `tart` that
/// dirtbag will use. A missing `tart` is reported, not an error: the user can
/// run this command to find out that Tart is not installed.
pub fn run() -> Result<()> {
    println!("dirtbag {}", env!("CARGO_PKG_VERSION"));

    match Tart::locate() {
        Ok(tart) => {
            let version = tart.version().unwrap_or_else(|_| "unknown".to_string());
            println!("tart {version} ({})", tart.bin().display());
        }
        Err(_) => println!("tart not found on PATH"),
    }

    Ok(())
}
