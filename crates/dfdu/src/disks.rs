//! `dfdu disks`: the mounted filesystems with their size and free space.

use std::fmt::Write as _;
use std::io;
use std::process::ExitCode;

use crate::print_out;

/// `dfdu disks`: one line per filesystem, like `df -h`.
pub fn run() -> Result<ExitCode, ExitCode> {
    let list = match dfdu_sys::disks(dfdu_sys::DISK_TIMEOUT) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("dfdu: cannot list disks: {e}");
            return Err(ExitCode::from(1));
        }
    };
    print_out(|w| table(w, &list))?;
    Ok(ExitCode::SUCCESS)
}

/// Writes `list` to `w`, a header and one line per filesystem.
fn table(w: &mut dyn io::Write, list: &[dfdu_sys::Disk]) -> io::Result<()> {
    let width = list
        .iter()
        .map(|d| d.mount_point.as_os_str().len())
        .max()
        .unwrap_or(0)
        .clamp(10, 40);
    // macOS: what the system can free on top of the free space, when some disk says.
    let purgeable = list
        .iter()
        .any(|d| d.space.is_some_and(|s| s.purgeable.is_some()));
    let mut header = format!(
        "{:<width$}  {:<8} {:>10} {:>10} {:>10}",
        "MOUNTED ON", "TYPE", "SIZE", "USED", "FREE"
    );
    if purgeable {
        let _ = write!(header, " {:>10}", "PURGEABLE");
    }
    writeln!(w, "{header}  USE%")?;
    for d in list {
        let mut line = format!("{:<width$}  {:<8}", d.mount_point.display(), d.fstype);
        match d.space {
            Some(s) => {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "a percentage shown to whole units"
                )]
                let pct = s.used as f64 * 100.0 / s.total.max(1) as f64;
                let _ = write!(
                    line,
                    " {:>10} {:>10} {:>10}",
                    dfdu_engine::human(s.total),
                    dfdu_engine::human(s.used),
                    dfdu_engine::human(s.available),
                );
                if purgeable {
                    let p = s.purgeable.map_or_else(|| "-".into(), dfdu_engine::human);
                    let _ = write!(line, " {p:>10}");
                }
                let _ = write!(line, "  {pct:>3.0}%");
            }
            None => line += "  not responding",
        }
        if d.class == dfdu_sys::FsClass::Network {
            line += "  network";
        }
        if let Some(role) = d.role {
            let _ = write!(line, "  {}", role.label());
        }
        if !d.also_at.is_empty() {
            let also: Vec<String> = d.also_at.iter().map(|p| p.display().to_string()).collect();
            let _ = write!(line, "  also at {}", also.join(", "));
        }
        if !d.shares_space_with.is_empty() {
            let others: Vec<String> = d
                .shares_space_with
                .iter()
                .map(|p| p.display().to_string())
                .collect();
            let _ = write!(line, "  shares space with {}", others.join(", "));
        }
        writeln!(w, "{line}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
