//! The settings of the viewer: a small text file that can be edited by hand and is also changed from the screen of the viewer (`s`).
//! They say how the viewer behaves; what happened to each book (the page, the zoom, the turns) is in the state file (`state.rs`).

use std::path::PathBuf;

use crate::files::Listing;
use crate::spread::Spread;

/// Whether the number of pages side by side and their height are the choice of each book or one choice for all books.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// `d` and `e` are remembered for the book on the screen; a book that was not given a choice opens as the defaults say.
    EachBook,
    /// `d` and `e` change the defaults, which every book follows.
    AllBooks,
}

/// How the pages of a scanned book (DjVu) are made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scans {
    /// With the pixels the window needs (`window`).
    Window,
    /// With twice the pixels the window needs, then shrunk as a whole page is (`finer`).
    Finer,
    /// At the scan's own size, then shrunk (`whole`, as before 0.5.0).
    Whole,
}

impl Scans {
    /// The word of the settings file.
    pub fn word(self) -> &'static str {
        match self {
            Self::Window => "window",
            Self::Finer => "finer",
            Self::Whole => "whole",
        }
    }

    /// The next one, for `v`: window, finer, whole, window.
    pub fn next(self) -> Self {
        match self {
            Self::Window => Self::Finer,
            Self::Finer => Self::Whole,
            Self::Whole => Self::Window,
        }
    }
}

/// What the author chose for one book with `d`, `e` and `o`: a thing that was not chosen is `None` and follows the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BookChoice {
    pub count: Option<usize>,
    pub equal_height: Option<bool>,
    pub cover_alone: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub scope: Scope,
    /// How many pages stand side by side (1, 2 or 3): for a book with no choice of its own, and for every book in `AllBooks`.
    pub pages: usize,
    pub equal_height: bool,
    /// The first page stands alone (the cover), and the pairs are 2-3, 4-5: for a book with no choice of its own.
    pub cover_alone: bool,
    /// Which files of the folder Space and Backspace go through: every one (a file that is no picture as a card), the pictures and documents,
    /// or the books only. `--cards` makes it every file for a run.
    pub browse: Listing,
    /// Folders of the user's own fonts as written (a list the way `PATH` is: `;` on Windows, `:` elsewhere); empty when none. Edited by
    /// hand, not on the screen of `s`.
    pub fonts: String,
    /// The size of the text of a book whose text flows (FB2, EPUB), in per cent of its own (`peekel_core::book::TEXT_SIZES`); one for
    /// every book.
    pub text_size: u32,
    /// The pages of books dark and their text light (`b`; `peekel_core::night`): for every book.
    pub night: bool,
    /// How the pages of a scanned book (DjVu) are made (`v` goes through them).
    pub scans: Scans,
    /// Where it is kept (nowhere: nothing is read or written).
    file: Option<PathBuf>,
}

impl Settings {
    /// The defaults and no file: how the viewer is without settings.
    pub fn none() -> Self {
        Self {
            scope: Scope::EachBook,
            pages: 2,
            equal_height: true,
            cover_alone: true,
            browse: Listing::Pictures,
            fonts: String::new(),
            text_size: 100,
            night: false,
            scans: Scans::Window,
            file: None,
        }
    }

    /// The folders of the user's own fonts the file names.
    pub fn font_dirs(&self) -> Vec<PathBuf> {
        std::env::split_paths(&self.fonts)
            .filter(|p| !p.as_os_str().is_empty())
            .collect()
    }

    /// What the file `settings` in `dir` says (the defaults for what it does not say or cannot be read); changes are written there.
    pub fn open(dir: PathBuf) -> Self {
        let file = dir.join("settings");
        let mut settings = Self::none();
        if let Ok(text) = std::fs::read_to_string(&file) {
            settings.parse(&text);
        }
        settings.file = Some(file);
        settings
    }

    /// The path of the file, for the screen to say where the settings are kept.
    pub fn path(&self) -> Option<&std::path::Path> {
        self.file.as_deref()
    }

    /// `name = value` lines; `#` begins a comment; a line that is not understood is passed over.
    fn parse(&mut self, text: &str) {
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or_default().trim();
            let Some((name, value)) = line.split_once('=') else {
                continue;
            };
            let raw = value.trim();
            let (name, value) = (name.trim().to_ascii_lowercase(), raw.to_ascii_lowercase());
            let yes_no = |v: &str| match v {
                "yes" | "true" | "on" => Some(true),
                "no" | "false" | "off" => Some(false),
                _ => None,
            };
            match name.as_str() {
                "spread" => match value.as_str() {
                    "each-book" => self.scope = Scope::EachBook,
                    "all-books" => self.scope = Scope::AllBooks,
                    _ => {}
                },
                "pages" => {
                    if let Some(n) = value.parse::<usize>().ok().filter(|n| (1..=3).contains(n)) {
                        self.pages = n;
                    }
                }
                "equal-height" => {
                    if let Some(v) = yes_no(&value) {
                        self.equal_height = v;
                    }
                }
                "cover-alone" => {
                    if let Some(v) = yes_no(&value) {
                        self.cover_alone = v;
                    }
                }
                "browse" => match value.as_str() {
                    "every" => self.browse = Listing::Every,
                    "pictures" => self.browse = Listing::Pictures,
                    "books" => self.browse = Listing::Books,
                    _ => {}
                },
                "text-size" => {
                    let (low, high) = peekel_core::book::TEXT_SIZES;
                    if let Some(n) = value
                        .trim_end_matches('%')
                        .trim()
                        .parse::<u32>()
                        .ok()
                        .filter(|n| (low..=high).contains(n))
                    {
                        self.text_size = n;
                    }
                }
                "night" => {
                    if let Some(v) = yes_no(&value) {
                        self.night = v;
                    }
                }
                "scans" => match value.as_str() {
                    "window" => self.scans = Scans::Window,
                    "finer" => self.scans = Scans::Finer,
                    "whole" => self.scans = Scans::Whole,
                    _ => {}
                },
                // a path keeps its capitals
                "fonts" => self.fonts = raw.to_string(),
                _ => {}
            }
        }
    }

    /// The file's text, with a word about each setting.
    fn text(&self) -> String {
        let yes_no = |v: bool| if v { "yes" } else { "no" };
        format!(
            "# The settings of peekel. Edit this file, or press s in the viewer. A line that is not understood is passed over.\n\
             \n\
             # each-book: d (pages side by side) and e (equal height) are remembered for each book; all-books: they are one choice for all.\n\
             spread = {}\n\
             \n\
             # How many pages stand side by side (1, 2 or 3): for a book that was given no choice of its own, and for every book in all-books.\n\
             pages = {}\n\
             \n\
             # The pages of a spread made as high as the tallest one (yes), or each in its own size (no).\n\
             equal-height = {}\n\
             \n\
             # The first page stands alone, then 2-3, 4-5 (yes); or the pairs are 1-2, 3-4 (no). o changes it for a book.\n\
             cover-alone = {}\n\
             \n\
             # Which files of the folder Space and Backspace go through: every (a file that is no picture is shown as a card: its kind, size,\n\
             # dates and what is inside), pictures (the pictures and documents the viewer reads), books (PDF, DjVu, FB2 and EPUB only).\n\
             browse = {}\n\
             \n\
             # The size of the text of FB2 and EPUB books and HTML pages, in per cent (70 to 200; f and F change it): the book is laid out again.\n\
             text-size = {}\n\
             \n\
             # The night look of books (PDF, DjVu, FB2, EPUB, HTML): the pages dark and the text light (yes), or as they are (no); b changes it.\n\
             night = {}\n\
             \n\
             # The pages of DjVu books (v goes through them): drawn for the window (window: the least memory, the quickest turning; a zoom\n\
             # draws the page again with more detail), with twice the pixels of the window and then shrunk (finer), or whole at the size of\n\
             # the scan and then shrunk (whole).\n\
             scans = {}\n\
             \n\
             # Folders of your own fonts (the originals copied from your Windows or Office), looked in first for the text of Word, Excel and\n\
             # PowerPoint pages and of cards; several folders are written the way PATH is (; on Windows, : elsewhere). Edit this file: it is not on the screen.\n\
             fonts = {}\n",
            match self.scope {
                Scope::EachBook => "each-book",
                Scope::AllBooks => "all-books",
            },
            self.pages,
            yes_no(self.equal_height),
            yes_no(self.cover_alone),
            match self.browse {
                Listing::Every => "every",
                Listing::Pictures => "pictures",
                Listing::Books => "books",
            },
            self.text_size,
            yes_no(self.night),
            self.scans.word(),
            self.fonts,
        )
    }

    /// Writes the file: a new one beside it first, then put in its place. A failure is not reported (the settings still hold for this
    /// run): a viewer that cannot write its settings must still show the book.
    pub fn save(&self) {
        let Some(file) = &self.file else { return };
        let Some(dir) = file.parent() else { return };
        let temp = file.with_extension("tmp");
        let written = std::fs::create_dir_all(dir)
            .and_then(|()| std::fs::write(&temp, self.text()))
            .and_then(|()| std::fs::rename(&temp, file));
        if written.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
    }

    /// How the pages of a book are put together: its own choices where it has them, the defaults elsewhere; in `AllBooks` the number of
    /// pages and their height are the defaults whatever the book chose.
    pub fn spread(&self, own: BookChoice) -> Spread {
        let (count, equal_height) = match self.scope {
            Scope::AllBooks => (self.pages, self.equal_height),
            Scope::EachBook => (
                own.count.unwrap_or(self.pages),
                own.equal_height.unwrap_or(self.equal_height),
            ),
        };
        Spread {
            count,
            equal_height,
            cover_alone: own.cover_alone.unwrap_or(self.cover_alone),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_file_the_defaults_are_two_pages_equally_high_the_cover_alone_for_each_book() {
        let s = Settings::none();
        assert_eq!(
            (s.scope, s.pages, s.equal_height, s.cover_alone),
            (Scope::EachBook, 2, true, true)
        );
        assert_eq!(s.spread(BookChoice::default()), Spread::DEFAULT);
    }

    #[test]
    fn the_file_is_read_with_comments_and_spaces_and_what_is_not_understood_is_passed_over() {
        let mut s = Settings::none();
        s.parse("# a comment\n  Spread = ALL-BOOKS   # for all\npages=3\nequal-height = no\ncover-alone = off\nnonsense\nunknown = 1\npages = 9\nspread = nowhere\n");
        assert_eq!(
            (s.scope, s.pages, s.equal_height, s.cover_alone),
            (Scope::AllBooks, 3, false, false),
            "pages = 9 and spread = nowhere are not understood and change nothing"
        );
    }

    #[test]
    fn the_files_gone_through_are_the_pictures_by_default_and_what_is_chosen_survives_a_save() {
        let mut s = Settings::none();
        assert_eq!(s.browse, Listing::Pictures);
        s.parse("browse = Books\n");
        assert_eq!(s.browse, Listing::Books);
        let mut again = Settings::none();
        again.parse(&s.text());
        assert_eq!(again.browse, Listing::Books);
        again.parse("browse = every\nbrowse = nonsense\n");
        assert_eq!(again.browse, Listing::Every, "a word not understood changes nothing");
    }

    #[test]
    fn the_size_of_the_text_is_its_own_by_default_kept_within_its_range_and_survives_a_save() {
        let mut s = Settings::none();
        assert_eq!(s.text_size, 100);
        s.parse("text-size = 130 %\n");
        assert_eq!(s.text_size, 130);
        s.parse("text-size = 500\ntext-size = big\ntext-size = 10\n");
        assert_eq!(s.text_size, 130, "out of the range or not a number: nothing changes");
        let mut again = Settings::none();
        again.parse(&s.text());
        assert_eq!(again.text_size, 130);
    }

    #[test]
    fn the_night_look_is_off_by_default_and_survives_a_save() {
        let mut s = Settings::none();
        assert!(!s.night);
        s.parse("night = yes\n");
        assert!(s.night);
        let mut again = Settings::none();
        again.parse(&s.text());
        assert!(again.night);
        again.parse("night = dusk\n");
        assert!(again.night, "a word not understood changes nothing");
    }

    #[test]
    fn scans_are_drawn_for_the_window_by_default_and_whole_survives_a_save() {
        let mut s = Settings::none();
        assert_eq!(s.scans, Scans::Window);
        for (word, scans) in [("Whole", Scans::Whole), ("finer", Scans::Finer)] {
            s.parse(&format!("scans = {word}\n"));
            assert_eq!(s.scans, scans);
            let mut again = Settings::none();
            again.parse(&s.text());
            assert_eq!(again.scans, scans);
            again.parse("scans = huge\n");
            assert_eq!(again.scans, scans, "a word not understood changes nothing");
        }
        // v goes through the three and back
        assert_eq!(Scans::Window.next().next().next(), Scans::Window);
        assert_eq!(Scans::Window.next(), Scans::Finer);
    }

    #[test]
    fn the_folders_of_fonts_keep_their_capitals_and_survive_a_save() {
        let mut s = Settings::none();
        s.parse("fonts = /Home/Me/Fonts\n");
        assert_eq!(s.font_dirs(), vec![PathBuf::from("/Home/Me/Fonts")]);
        let mut again = Settings::none();
        again.parse(&s.text());
        assert_eq!(again.fonts, "/Home/Me/Fonts");
        assert!(Settings::none().font_dirs().is_empty(), "no line, no folder");
    }

    #[test]
    fn what_is_saved_is_read_back_and_a_folder_that_cannot_be_written_does_not_stop_anything() {
        let dir = std::env::temp_dir().join(format!("peekel-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut s = Settings::open(dir.clone());
        assert_eq!(s.pages, 2, "no file yet: the defaults");
        s.scope = Scope::AllBooks;
        s.pages = 3;
        s.equal_height = false;
        s.cover_alone = false;
        s.save();
        let again = Settings::open(dir.clone());
        assert_eq!(
            (again.scope, again.pages, again.equal_height, again.cover_alone),
            (Scope::AllBooks, 3, false, false)
        );
        assert!(std::fs::read_to_string(dir.join("settings")).is_ok_and(|t| t.contains("spread = all-books")));
        let _ = std::fs::remove_dir_all(dir);
        Settings::none().save(); // no folder: nothing happens
    }

    #[test]
    fn each_book_follows_its_own_choices_and_all_books_follows_the_defaults_except_for_the_cover() {
        let mut s = Settings::none();
        let own = BookChoice {
            count: Some(3),
            equal_height: Some(false),
            cover_alone: Some(false),
        };
        assert_eq!(
            s.spread(own),
            Spread {
                count: 3,
                equal_height: false,
                cover_alone: false
            }
        );
        // a choice that was not made follows the default
        let only_cover = BookChoice {
            cover_alone: Some(false),
            ..BookChoice::default()
        };
        assert_eq!(
            s.spread(only_cover),
            Spread {
                cover_alone: false,
                ..Spread::DEFAULT
            }
        );
        s.scope = Scope::AllBooks;
        s.pages = 1;
        assert_eq!(
            s.spread(own),
            Spread {
                count: 1,
                equal_height: true,
                cover_alone: false
            },
            "the pages and the height are the defaults; the cover is the book's own"
        );
    }
}
