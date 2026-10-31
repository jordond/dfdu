//! The category taxonomy: every kind of disk use a rule can file its matches under, and the
//! theme colour slot each is drawn in.

/// The theme's colour slots for categories (`cat.*` tokens), in their order.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CategoryColor {
    Cache,
    Build,
    Vm,
    Toolchain,
    Media,
    Download,
    Container,
    Other,
}

/// One category of the taxonomy.
#[derive(Debug)]
pub struct Category {
    pub id: &'static str,
    pub name: &'static str,
    pub color: CategoryColor,
}

const fn cat(id: &'static str, name: &'static str, color: CategoryColor) -> Category {
    Category { id, name, color }
}

/// Every category, in the taxonomy's order. A rule's `category` must be one of these ids.
pub const CATEGORIES: &[Category] = &[
    cat("dev.build", "Project build output", CategoryColor::Build),
    cat("dev.deps", "Project dependencies", CategoryColor::Build),
    cat(
        "dev.pkgcache",
        "Package manager caches",
        CategoryColor::Cache,
    ),
    cat(
        "dev.xcode",
        "Xcode and Apple simulators",
        CategoryColor::Toolchain,
    ),
    cat(
        "dev.android",
        "Android SDK and emulators",
        CategoryColor::Vm,
    ),
    cat(
        "dev.toolchains",
        "Toolchains and runtimes",
        CategoryColor::Toolchain,
    ),
    cat(
        "dev.containers",
        "Containers and VMs",
        CategoryColor::Container,
    ),
    cat("dev.ide", "IDE and editor caches", CategoryColor::Cache),
    cat("dev.nix", "Nix", CategoryColor::Toolchain),
    cat("sys.cache", "OS and user caches", CategoryColor::Cache),
    cat("sys.logs", "Logs", CategoryColor::Other),
    cat("sys.crash", "Crash reports and dumps", CategoryColor::Other),
    cat("sys.trash", "Trash", CategoryColor::Download),
    cat(
        "sys.updates",
        "OS update leftovers",
        CategoryColor::Download,
    ),
    cat("sys.snapshots", "Snapshots", CategoryColor::Other),
    cat("app.browser", "Browser caches", CategoryColor::Cache),
    cat("app.chat", "Chat and meeting apps", CategoryColor::Cache),
    cat("app.media", "Media apps", CategoryColor::Media),
    cat("app.games", "Games", CategoryColor::Media),
    cat("app.mail", "Mail", CategoryColor::Other),
    cat("app.mobile", "Mobile device data", CategoryColor::Vm),
    cat(
        "content.large-old",
        "Large, not recently used files",
        CategoryColor::Other,
    ),
    cat(
        "content.duplicates",
        "Duplicate files",
        CategoryColor::Other,
    ),
    cat(
        "content.installers",
        "Installers and disk images",
        CategoryColor::Download,
    ),
    cat("content.media", "Large video files", CategoryColor::Media),
    cat(
        "content.extracted",
        "Archives already extracted",
        CategoryColor::Download,
    ),
    cat(
        "content.empty",
        "Empty files and folders",
        CategoryColor::Other,
    ),
    cat(
        "content.broken-links",
        "Broken symbolic links",
        CategoryColor::Other,
    ),
];

/// A category, as its place in [`CATEGORIES`]: ordered as the taxonomy is.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct CategoryIx(u16);

impl CategoryIx {
    /// The category with id `id`, `None` for an id outside the taxonomy.
    #[must_use]
    pub fn of(id: &str) -> Option<CategoryIx> {
        Self::all().find(|c| c.get().id == id)
    }

    /// The categories `name` stands for: the one with that id, or every one of a family
    /// (`dev` for `dev.build`, `dev.deps` and the rest). Empty when it names none.
    #[must_use]
    pub fn named(name: &str) -> Vec<CategoryIx> {
        Self::all()
            .filter(|c| {
                let id = c.get().id;
                id == name || id.split('.').next() == Some(name)
            })
            .collect()
    }

    /// Every category, in the taxonomy's order.
    pub fn all() -> impl Iterator<Item = CategoryIx> {
        (0..CATEGORIES.len()).filter_map(|i| u16::try_from(i).ok().map(CategoryIx))
    }

    #[must_use]
    pub fn get(self) -> &'static Category {
        &CATEGORIES[usize::from(self.0)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories() {
        assert_eq!(
            CategoryIx::of("dev.build").map(|c| c.get().id),
            Some("dev.build")
        );
        assert_eq!(CategoryIx::of(""), None);
        assert_eq!(CategoryIx::of("dev.nope"), None);
        assert_eq!(CategoryIx::of("sys.trash").unwrap().get().name, "Trash");
        assert_eq!(CategoryIx::all().count(), CATEGORIES.len());
        assert!(CategoryIx::of("dev.build") < CategoryIx::of("sys.trash"));
        let dev = CategoryIx::named("dev");
        assert!(dev.len() > 1 && dev.iter().all(|c| c.get().id.starts_with("dev.")));
        assert_eq!(
            CategoryIx::named("sys.logs"),
            CategoryIx::of("sys.logs").into_iter().collect::<Vec<_>>()
        );
        assert!(CategoryIx::named("de").is_empty() && CategoryIx::named("").is_empty());
    }
}
