//! `dfdu disks`: the mounted filesystems with their size and free space.

use std::fmt::Write as _;
use std::process::ExitCode;

/// `dfdu disks`: one line per filesystem, like `df -h`.
pub fn run() -> ExitCode {
    let list = match dfdu_sys::disks(dfdu_sys::DISK_TIMEOUT) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("dfdu: cannot list disks: {e}");
            return ExitCode::from(1);
        }
    };
    let width = list
        .iter()
        .map(|d| d.mount_point.as_os_str().len())
        .max()
        .unwrap_or(0)
        .clamp(10, 40);
    outln!(
        "{:<width$}  {:<8} {:>10} {:>10} {:>10}  {:>4}",
        "MOUNTED ON",
        "TYPE",
        "SIZE",
        "USED",
        "FREE",
        "USE%"
    );
    for d in &list {
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
                    " {:>10} {:>10} {:>10}  {pct:>3.0}%",
                    dfdu_engine::human(s.total),
                    dfdu_engine::human(s.used),
                    dfdu_engine::human(s.available),
                );
            }
            None => line += "  not responding",
        }
        if d.class == dfdu_sys::FsClass::Network {
            line += "  network";
        }
        if !d.also_at.is_empty() {
            let also: Vec<String> = d.also_at.iter().map(|p| p.display().to_string()).collect();
            let _ = write!(line, "  also at {}", also.join(", "));
        }
        outln!("{line}");
    }
    ExitCode::SUCCESS
}
