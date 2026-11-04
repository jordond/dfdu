//! macOS firmlinks: directories of the read-only system volume that show a directory of the
//! Data volume (`/Users` shows `/System/Volumes/Data/Users`). Rules and protected paths are
//! written for the paths the system volume shows, so a path in the Data volume is read through
//! the firmlink that shows it: `/System/Volumes/Data/Users/me/Library/Caches` is
//! `/Users/me/Library/Caches` to the rules. Data volume paths no firmlink shows stay as they
//! are, under the protected `/System`.

use std::sync::{Arc, LazyLock};

/// Where macOS mounts the Data volume.
pub const DATA_VOLUME: &str = "/System/Volumes/Data";

/// The firmlinks of a machine: each directory of the Data volume a firmlink shows, with the
/// path it is shown at. Empty off macOS.
#[derive(Clone, Debug, Default)]
pub struct Firmlinks(Arc<[Link]>);

#[derive(Debug)]
struct Link {
    /// Where the system volume shows it (`/Users`).
    shown: Vec<u8>,
    /// The directory in the Data volume (`/System/Volumes/Data/Users`).
    data: Vec<u8>,
}

impl Firmlinks {
    /// The firmlinks listed in `text`, as `/usr/share/firmlinks` has them: one per line, the
    /// path on the system volume, a tab, and the directory relative to the Data volume.
    #[must_use]
    pub fn parse(text: &str) -> Firmlinks {
        let links: Vec<Link> = text
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .map(|(shown, data)| (shown.trim_end_matches('/'), data.trim_matches('/')))
            .filter(|(shown, data)| shown.len() > 1 && shown.starts_with('/') && !data.is_empty())
            .map(|(shown, data)| Link {
                shown: shown.as_bytes().to_vec(),
                data: format!("{DATA_VOLUME}/{data}").into_bytes(),
            })
            .collect();
        Firmlinks(links.into())
    }

    /// This machine's: `/usr/share/firmlinks` on macOS (10.15 and later), read once; none
    /// elsewhere or when it cannot be read.
    #[must_use]
    pub fn local() -> Firmlinks {
        static LOCAL: LazyLock<Firmlinks> = LazyLock::new(|| {
            if cfg!(target_os = "macos") {
                std::fs::read_to_string("/usr/share/firmlinks")
                    .map(|t| Firmlinks::parse(&t))
                    .unwrap_or_default()
            } else {
                Firmlinks::default()
            }
        });
        LOCAL.clone()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `path` (`/`-separated) as the system volume shows it: when it is a directory of the
    /// Data volume a firmlink shows, or below one, with that directory replaced by where it is
    /// shown (the longest such directory); as it is otherwise.
    #[must_use]
    pub fn shown(&self, path: Vec<u8>) -> Vec<u8> {
        if !path.starts_with(DATA_VOLUME.as_bytes()) {
            return path;
        }
        let link = (self.0.iter())
            .filter(|l| {
                path.starts_with(&l.data)
                    && (path.len() == l.data.len() || path[l.data.len()] == b'/')
            })
            .max_by_key(|l| l.data.len());
        match link {
            Some(l) => {
                let mut p = l.shown.clone();
                p.extend_from_slice(&path[l.data.len()..]);
                p
            }
            None => path,
        }
    }

    /// Where the system volume shows `path` when it is exactly a directory of the Data volume
    /// a firmlink shows (`/Users` for `/System/Volumes/Data/Users`); `None` for every other
    /// path, those below such a directory included.
    #[must_use]
    pub fn shown_at(&self, path: &[u8]) -> Option<&[u8]> {
        if !path.starts_with(DATA_VOLUME.as_bytes()) {
            return None;
        }
        (self.0.iter())
            .find(|l| l.data == path)
            .map(|l| l.shown.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links() -> Firmlinks {
        Firmlinks::parse(
            "/Users\tUsers\n/Library\tLibrary\n/usr/local\tusr/local\n\
             /System/Library/Caches\tSystem/Library/Caches\nnot a link\n",
        )
    }

    fn shown(p: &str) -> String {
        String::from_utf8(links().shown(p.as_bytes().to_vec())).unwrap_or_default()
    }

    #[test]
    fn data_volume_paths_are_read_where_a_firmlink_shows_them() {
        assert_eq!(shown("/System/Volumes/Data/Users"), "/Users");
        assert_eq!(shown("/System/Volumes/Data/Users/me/x"), "/Users/me/x");
        assert_eq!(
            shown("/System/Volumes/Data/usr/local/bin"),
            "/usr/local/bin"
        );
        assert_eq!(
            shown("/System/Volumes/Data/System/Library/Caches/com.apple.x"),
            "/System/Library/Caches/com.apple.x"
        );
    }

    #[test]
    fn paths_no_firmlink_shows_stay_as_they_are() {
        // The Data volume itself, what no firmlink shows, and names that only start alike.
        for p in [
            "/System/Volumes/Data",
            "/System/Volumes/Data/.Spotlight-V100",
            "/System/Volumes/Data/usr",
            "/System/Volumes/Data/UsersX",
            "/System/Volumes/Data/System/Library",
            "/Users/me",
            "/System/Volumes/Preboot/Users",
        ] {
            assert_eq!(shown(p), p);
        }
    }

    #[test]
    fn only_a_linked_directory_itself_is_where_a_firmlink_starts() {
        let l = links();
        assert_eq!(
            l.shown_at(b"/System/Volumes/Data/Users"),
            Some(&b"/Users"[..])
        );
        assert_eq!(
            l.shown_at(b"/System/Volumes/Data/usr/local"),
            Some(&b"/usr/local"[..])
        );
        assert_eq!(l.shown_at(b"/System/Volumes/Data/Users/me"), None);
        assert_eq!(l.shown_at(b"/System/Volumes/Data/usr"), None);
        assert_eq!(l.shown_at(b"/Users"), None);
    }

    #[test]
    fn none_without_a_firmlinks_file() {
        let l = Firmlinks::default();
        assert!(l.is_empty());
        assert_eq!(
            l.shown(b"/System/Volumes/Data/Users/me".to_vec()),
            b"/System/Volumes/Data/Users/me"
        );
    }
}
