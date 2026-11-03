//! The breadcrumbs of the toolbar: the directories from the top of the filesystem (or `~`) to
//! the current one, those above the scan root dimmer, folded into `…` when they do not fit. A
//! click on one below the root opens it; on one above the root it asks for a new scan.

use std::path::Path;

use dfdu_session::SessionState;
use dfdu_session::palette::Token;
use dfdu_tree::{Host, HostPath};
use egui::Ui;

use crate::browser::Intent;
use crate::kit;
use crate::theme::Colors;

/// Between two breadcrumbs.
const SEPARATOR: &str = "/";

/// One breadcrumb: a directory on the way to the current one.
#[derive(Debug, PartialEq)]
pub struct Segment {
    pub text: String,
    pub dir: HostPath,
    /// Above the scan root: opening it is a new scan.
    pub above: bool,
}

/// The breadcrumbs of `dir`: the directories above the scan root `root` (from the root of
/// the filesystem, or from the home directory `home` as `~` when the scan root is in it), the
/// scan root, then one per directory below it. All of `dir` is one breadcrumb when it is not
/// below `root` (before the first snapshot).
pub fn segments(dir: &HostPath, root: &HostPath, home: Option<&Path>) -> Vec<Segment> {
    let below = match dir.rel(root) {
        Some(below) if !root.is_empty() => below,
        _ => {
            return vec![Segment {
                text: dfdu_engine::tilde(dir, home),
                dir: dir.clone(),
                above: false,
            }];
        }
    };
    let mut out = ancestors(root, home);
    let mut dir = root.clone();
    for part in below {
        dir.push(part);
        out.push(Segment {
            text: String::from_utf8_lossy(part).into_owned(),
            dir: dir.clone(),
            above: false,
        });
    }
    out
}

/// The breadcrumbs from the top down to the scan root `root`, which is the last: each
/// directory by its name, the root of the filesystem by its path, and the home directory
/// `home` as `~`, where they stop. The home directory counts on a scan of this machine only,
/// and not when it is `/`, as in [`dfdu_engine::tilde`].
pub fn ancestors(root: &HostPath, home: Option<&Path>) -> Vec<Segment> {
    let home =
        (home.filter(|h| h.parent().is_some() && root.host() == Host::LOCAL)).map(HostPath::local);
    let mut out = Vec::new();
    let mut at = root.clone();
    loop {
        if home.as_ref() == Some(&at) {
            out.push(Segment {
                text: "~".into(),
                dir: at,
                above: true,
            });
            break;
        }
        let text = match at.file_name() {
            Some(name) => String::from_utf8_lossy(name).into_owned(),
            None => at.display().into_owned(),
        };
        let up = at.parent();
        out.push(Segment {
            text,
            dir: at,
            above: true,
        });
        match up {
            Some(up) => at = up,
            None => break,
        }
    }
    out.reverse();
    if let Some(last) = out.last_mut() {
        last.above = false;
    }
    out
}

/// Whether a separator is drawn after the breadcrumb `text`: not after the root of the
/// filesystem, which ends with one.
fn separated(text: &str, host: Host) -> bool {
    !text.bytes().last().is_some_and(|b| host.is_sep(b))
}

/// Which breadcrumbs fit in `room` when each is `widths[i]` wide and a separator `sep`:
/// the first and the last ones always, the middle ones from the end while they fit, the
/// others one `…` (`None`).
pub fn fitting(widths: &[f32], sep: f32, ellipsis: f32, room: f32) -> Vec<Option<usize>> {
    let n = widths.len();
    let total: f32 = widths.iter().sum::<f32>() + sep * n.saturating_sub(1) as f32;
    if total <= room || n <= 2 {
        return (0..n).map(Some).collect();
    }
    let mut used = widths[0] + widths[n - 1] + 2.0 * sep + ellipsis + sep;
    let mut kept = Vec::new();
    for i in (1..n - 1).rev() {
        if used + widths[i] + sep > room {
            break;
        }
        used += widths[i] + sep;
        kept.push(i);
    }
    kept.reverse();
    let mut out = vec![Some(0)];
    if kept.first() != Some(&1) {
        out.push(None);
    }
    out.extend(kept.into_iter().map(Some));
    out.push(Some(n - 1));
    out
}

/// The breadcrumbs in mono: the directories above as dim links, those above the scan root
/// dimmer, the current one in `dir`. When they do not fit, the middle ones fold into `…` and
/// the current one is cut short. A click on one above the scan root asks for a new scan of it;
/// on a scan read from a file they are not links.
pub fn breadcrumbs(ui: &mut Ui, s: &SessionState, colors: &Colors, out: &mut Vec<Intent>) {
    let snap = &s.snap;
    let host = snap.dir_path.host();
    let segs = segments(&snap.dir_path, &snap.root_path, s.home.as_deref());
    let last = segs.len() - 1;
    let gap = 6.0;
    ui.spacing_mut().item_spacing.x = gap;
    let font = egui::FontId::new(13.0, egui::FontFamily::Monospace);
    let width = |t: &str| {
        ui.painter()
            .layout_no_wrap(t.to_owned(), font.clone(), egui::Color32::WHITE)
            .size()
            .x
    };
    let widths: Vec<f32> = segs.iter().map(|g| width(&g.text)).collect();
    let sep = width(SEPARATOR) + 2.0 * gap;
    let room = ui.available_width();
    let shown = fitting(&widths, sep, width("…"), room);
    let mut after_sep = true;
    for (k, at) in shown.iter().enumerate() {
        if k > 0 && after_sep {
            ui.label(kit::mono(SEPARATOR, 13.0).color(colors.fg(Token::Border)));
        }
        after_sep = at.is_none_or(|i| separated(&segs[i].text, host));
        let Some(i) = *at else {
            ui.label(kit::mono("…", 13.0).color(colors.fg(Token::FgDim)));
            continue;
        };
        let seg = &segs[i];
        if i == last {
            let label =
                egui::Label::new(kit::mono_bold(&seg.text, 13.0).color(colors.fg(Token::Dir)))
                    .truncate();
            ui.add(label).on_hover_text(seg.text.as_str());
            break;
        }
        if seg.above {
            let text = kit::mono(&seg.text, 13.0).color(colors.fg(Token::Border));
            if s.from_file.is_some() {
                ui.label(text);
                continue;
            }
            let link = ui.add(egui::Link::new(text));
            if link.on_hover_text(format!("Scan {}", seg.dir)).clicked() {
                out.push(Intent::Reopen(seg.dir.clone()));
            }
            continue;
        }
        let link = egui::Link::new(kit::mono(&seg.text, 13.0).color(colors.fg(Token::FgDim)));
        if ui.add(link).clicked() {
            out.push(Intent::GoTo {
                child: segs[i + 1].dir.clone(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breadcrumbs_run_from_the_root_with_home_as_tilde() {
        let home = Path::new("/home/u");
        let root = HostPath::from("/home/u/dev");
        // Joined as the tree joins them: with `\` on Windows.
        let dir = root.join(b"a").join(b"b");
        let segs = segments(&dir, &root, Some(home));
        let texts: Vec<&str> = segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["~", "dev", "a", "b"]);
        assert_eq!(segs[2].dir, root.join(b"a"));
        let above: Vec<bool> = segs.iter().map(|s| s.above).collect();
        assert_eq!(above, [true, false, false, false]);
        assert_eq!(segs[0].dir, HostPath::local(home));
        // Before the first snapshot the root is not known: the directory is one breadcrumb.
        let segs = segments(&dir, &HostPath::from(""), Some(home));
        assert_eq!(segs.len(), 1);
    }

    /// The texts of the breadcrumbs above and down to `root`, and which are above it.
    fn above(root: &HostPath, home: Option<&Path>) -> (Vec<String>, Vec<bool>) {
        let segs = ancestors(root, home);
        (
            segs.iter().map(|s| s.text.clone()).collect(),
            segs.iter().map(|s| s.above).collect(),
        )
    }

    #[test]
    fn the_directories_above_the_scan_root_come_before_it() {
        let root = HostPath::new(Host::Unix, b"/mnt/cache".to_vec());
        let (texts, up) = above(&root, None);
        assert_eq!(texts, ["/", "mnt", "cache"]);
        assert_eq!(up, [true, true, false]);
        assert_eq!(ancestors(&root, None)[1].dir.as_bytes(), b"/mnt");
        // The root of the filesystem is the scan root, with nothing above.
        let top = HostPath::new(Host::Unix, b"/".to_vec());
        assert_eq!(above(&top, None), (vec!["/".to_owned()], vec![false]));
        // The home directory itself is `~`, with nothing above it shown.
        let home = Path::new("/home/u");
        assert_eq!(
            above(&HostPath::local(home), Some(home)),
            (vec!["~".to_owned()], vec![false])
        );
        // No separator after the root of the filesystem, which ends with one.
        assert!(!separated("/", Host::Unix) && separated("mnt", Host::Unix));
        assert!(!separated("C:\\", Host::Windows));
    }

    #[test]
    fn a_windows_scan_root_starts_at_its_drive() {
        let root = HostPath::new(Host::Windows, b"C:\\Users\\x".to_vec());
        let (texts, up) = above(&root, None);
        assert_eq!(texts, ["C:\\", "Users", "x"]);
        assert_eq!(up, [true, true, false]);
    }

    #[test]
    fn breadcrumbs_that_do_not_fit_fold_the_middle_ones() {
        let w = [50.0, 30.0, 30.0, 30.0, 40.0];
        assert_eq!(fitting(&w, 10.0, 10.0, 1000.0).len(), 5);
        // Room for the ends, the ellipsis and one more.
        assert_eq!(
            fitting(&w, 10.0, 10.0, 50.0 + 40.0 + 30.0 + 10.0 + 4.0 * 10.0),
            [Some(0), None, Some(3), Some(4)]
        );
        assert_eq!(fitting(&w, 10.0, 10.0, 0.0), [Some(0), None, Some(4)]);
    }
}
