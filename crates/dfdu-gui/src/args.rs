//! The command line: `dfdu-gui [PATH] [--read-only] [--import FILE] [--no-rules] [--theme NAME]`,
//! the flags of `dfdu` a launcher or a test needs, spelled as `dfdu` spells them.

use std::path::PathBuf;

use clap::Parser;

/// Disk usage browser for the desktop. Without PATH, the start screen lists the disks.
#[derive(Debug, Parser, PartialEq, Eq)]
#[command(name = "dfdu-gui", version, about)]
pub struct Args {
    /// Directory to browse.
    pub path: Option<PathBuf>,
    /// Never delete anything.
    #[arg(long)]
    pub read_only: bool,
    /// Browse a scan saved in FILE (a snapshot, an ncdu JSON export, plain or zstd, or an ncdu
    /// binary export), read-only.
    #[arg(long, value_name = "FILE", conflicts_with = "path")]
    pub import: Option<PathBuf>,
    /// Do not classify entries: no categories, no cleanup candidates.
    #[arg(long)]
    pub no_rules: bool,
    /// Colour theme: auto (the system's dark or light), dark, light, or NAME for the theme file
    /// NAME.toml in dfdu/themes in the configuration directory. A terminal's ansi, mono and
    /// high-contrast follow the system, as auto does. Without it, the theme chosen last.
    #[arg(long, value_name = "NAME")]
    pub theme: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, clap::Error> {
        Args::try_parse_from(std::iter::once("dfdu-gui").chain(args.iter().copied()))
    }

    #[test]
    fn no_arguments_open_the_start_screen() {
        let a = parse(&[]).expect("parses");
        assert_eq!(
            a,
            Args {
                path: None,
                read_only: false,
                import: None,
                no_rules: false,
                theme: None,
            }
        );
    }

    #[test]
    fn the_flags_are_spelled_as_dfdu_spells_them() {
        let a = parse(&["/tmp/x", "--read-only", "--no-rules"]).expect("parses");
        assert_eq!(a.path.as_deref(), Some(std::path::Path::new("/tmp/x")));
        assert!(a.read_only && a.no_rules && a.import.is_none());
        let a = parse(&["--theme", "nord"]).expect("parses");
        assert_eq!(a.theme.as_deref(), Some("nord"));
        let a = parse(&["--import", "scan.json"]).expect("parses");
        assert_eq!(a.import.as_deref(), Some(std::path::Path::new("scan.json")));
    }

    #[test]
    fn an_import_takes_no_path_and_unknown_flags_are_refused() {
        assert!(parse(&["/tmp/x", "--import", "scan.json"]).is_err());
        assert!(parse(&["--remote", "host"]).is_err());
        assert!(parse(&["a", "b"]).is_err());
    }
}
