//! The pictures next to the one that was opened: Far passes one file, but a viewer moves through the folder.

use std::path::{Path, PathBuf};

/// What `core::decode` reads (the formats the `image` crate is built with).
const EXTENSIONS: [&str; 10] = ["png", "jpg", "jpeg", "jfif", "gif", "bmp", "webp", "tif", "tiff", "dds"];

/// Whether the name looks like a picture this program reads.
pub fn is_picture(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        || peekel_core::decode::is_special(path)
}

/// The endings of the names this program reads (without the dot, small letters, each once): the plain pictures, then the formats of
/// `core`'s own.
pub fn endings() -> Vec<&'static str> {
    let mut all: Vec<&'static str> = Vec::new();
    for ending in EXTENSIONS.into_iter().chain(peekel_core::decode::special_endings()) {
        if !all.contains(&ending) {
            all.push(ending);
        }
    }
    all
}

/// The mask of Far Manager's file associations for these files: `*.png,*.jpg,...`.
pub fn far_mask() -> String {
    endings().iter().map(|e| format!("*.{e}")).collect::<Vec<_>>().join(",")
}

/// The section of Midnight Commander's `mc.ext.ini` that opens these files with `peekel` (a key file of GLib, where `\\` is one
/// backslash of the expression).
pub fn mc_section() -> String {
    mc_section_for("peekel")
}

/// As [`mc_section`], the program called as `command`. F3 and Enter both: mc takes the first section that matches and looks no
/// further, so Enter would else do nothing for these files (`install.rs`).
pub fn mc_section_for(command: &str) -> String {
    let names: Vec<String> = endings().iter().map(|e| e.replace('.', "\\\\.")).collect();
    format!(
        "[Peekel]\nRegex=\\\\.({})$\nRegexIgnoreCase=true\nOpen={command} %f\nView={command} %f\n",
        names.join("|")
    )
}

/// Which files of the folder Space and Backspace go through (the setting `browse`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listing {
    /// Every file: one that is no picture is shown as a card.
    Every,
    /// The pictures and the documents the viewer reads (the default).
    Pictures,
    /// The books only (PDF, DjVu, FB2 and EPUB): the viewer as a reader.
    Books,
}

impl Listing {
    fn keeps(self, path: &Path) -> bool {
        match self {
            Self::Every => true,
            Self::Pictures => is_picture(path),
            Self::Books => {
                peekel_core::pdf::is_pdf(path)
                    || peekel_core::djvu::is_djvu(path)
                    || peekel_core::fb2::is_fb2(path)
                    || peekel_core::epub::is_epub(path)
            }
        }
    }
}

/// The files of the folder of `path` that `listing` keeps, by name (capitals do not matter), and where `path` is among them. The file itself
/// is in the list whatever it is, and a folder that cannot be read gives the file alone.
pub fn in_folder(path: &Path, listing: Listing) -> (Vec<PathBuf>, usize) {
    let folder = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let name_of = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(&folder)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| listing.keeps(p) && p.is_file())
                .collect()
        })
        .unwrap_or_default();
    let wanted = name_of(path);
    if !files.iter().any(|p| name_of(p) == wanted) {
        files.push(path.to_path_buf());
    }
    files.sort_by_key(|p| name_of(p));
    let index = files.iter().position(|p| name_of(p) == wanted).unwrap_or(0);
    // the path as the user gave it, not as the folder lists it
    if let Some(slot) = files.get_mut(index) {
        *slot = path.to_path_buf();
    }
    (files, index)
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its folder should stop at once
mod tests {
    use super::*;

    #[test]
    fn the_masks_name_every_format_once_and_only_what_is_read() {
        let endings = endings();
        for wanted in [
            "png", "jpg", "dds", "nef", "svgz", "pdf", "djvu", "fb2.zip", "epub", "htm", "xlsx", "odp",
        ] {
            assert!(endings.contains(&wanted), "{wanted}");
        }
        for (i, e) in endings.iter().enumerate() {
            assert!(!endings[i + 1..].contains(e), "{e} twice");
            assert!(is_picture(Path::new(&format!("x.{e}"))), "{e}");
        }
        assert!(far_mask().starts_with("*.png,*.jpg,"));
        assert!(far_mask().contains(",*.fb2.zip,"));
        let mc = mc_section();
        assert!(mc.contains("|fb2\\\\.zip|"), "{mc}");
        assert!(mc.starts_with("[Peekel]\nRegex=\\\\.(png|jpg|"), "{mc}");
    }

    /// The README of the viewer shows the same masks as `--masks`: here in `public-viewer`, at the root in the public copy.
    #[test]
    fn the_readme_gives_the_masks_the_program_prints() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let Some(readme) = [root.join("../public-viewer/README.md"), root.join("../README.md")]
            .into_iter()
            .find(|p| p.is_file())
        else {
            panic!("no README beside the crate");
        };
        let text = std::fs::read_to_string(&readme)
            .unwrap_or_default()
            .replace("\r\n", "\n");
        assert!(
            text.contains(&far_mask()),
            "{}: the mask of Far is not {}",
            readme.display(),
            far_mask()
        );
        let section: String = mc_section().lines().map(|l| format!("  {l}\n")).collect();
        assert!(
            text.contains(&section),
            "{}: the section of mc is not\n{section}",
            readme.display()
        );
    }

    /// A folder of its own under the system's temporary one, with the named empty files in it.
    fn folder_with(name: &str, files: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("peekel-files-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("makes the folder");
        for file in files {
            std::fs::write(dir.join(file), b"").expect("makes the file");
        }
        dir
    }

    fn names(files: &[PathBuf]) -> Vec<String> {
        files
            .iter()
            .map(|p| p.file_name().expect("a name").to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn the_pictures_of_the_folder_by_name_without_regard_to_capitals_and_the_opened_one_found() {
        let dir = folder_with("sorted", &["b.PNG", "A.jpg", "c.txt", "d.TIFF", "e.webp", "notes"]);
        let (files, index) = in_folder(&dir.join("b.PNG"), Listing::Pictures);
        assert_eq!(names(&files), ["A.jpg", "b.PNG", "d.TIFF", "e.webp"]);
        assert_eq!(index, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_does_not_look_like_a_picture_is_still_in_the_list_so_that_it_can_be_tried() {
        let dir = folder_with("odd-name", &["a.png", "photo"]);
        let (files, index) = in_folder(&dir.join("photo"), Listing::Pictures);
        assert_eq!(names(&files), ["a.png", "photo"]);
        assert_eq!(index, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_that_cannot_be_read_gives_the_file_alone() {
        let (files, index) = in_folder(Path::new("no-such-folder-here/x.png"), Listing::Pictures);
        assert_eq!((names(&files), index), (vec!["x.png".to_string()], 0));
    }

    #[test]
    fn a_bare_name_means_the_current_folder() {
        let (files, index) = in_folder(Path::new("Cargo.toml"), Listing::Pictures);
        assert_eq!(names(&files), ["Cargo.toml"]);
        assert_eq!(index, 0);
    }

    #[test]
    fn with_all_every_file_of_the_folder_is_in_the_list() {
        let dir = folder_with("all", &["b.png", "a.ttf", "c.txt"]);
        let (files, index) = in_folder(&dir.join("b.png"), Listing::Every);
        assert_eq!(names(&files), ["a.ttf", "b.png", "c.txt"]);
        assert_eq!(index, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn with_books_only_the_pdf_djvu_fb2_and_epub_files_and_the_one_opened() {
        let dir = folder_with(
            "books",
            &["a.pdf", "b.jpg", "c.DJVU", "d.zip", "e.fb2", "f.fb2.zip", "g.epub"],
        );
        let (files, index) = in_folder(&dir.join("b.jpg"), Listing::Books);
        assert_eq!(
            names(&files),
            ["a.pdf", "b.jpg", "c.DJVU", "e.fb2", "f.fb2.zip", "g.epub"]
        );
        assert_eq!(index, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn folders_that_look_like_pictures_are_not_pictures() {
        let dir = folder_with("dirs", &["a.png"]);
        std::fs::create_dir(dir.join("b.png")).expect("makes a folder");
        let (files, _) = in_folder(&dir.join("a.png"), Listing::Pictures);
        assert_eq!(names(&files), ["a.png"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
