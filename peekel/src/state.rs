//! What the viewer remembers about each book between runs: the page, how its pages are put together, the zoom and the turns. A small
//! text file in the user's own folder for such things (never a file beside the book).

use std::path::{Path, PathBuf};

use peekel_core::view::Relative;

const HEADER: &str = "peekel-state 1";

/// The books remembered; the oldest are forgotten after these many.
const MAX_BOOKS: usize = 200;

/// What is remembered about one book (a file of many pages).
#[derive(Debug, Clone, PartialEq)]
pub struct Book {
    /// The size of the file when it was left: a file that has changed is another book.
    pub size: u64,
    /// The page the spread on the screen began with.
    pub page: usize,
    /// What the author chose for this book with `o`, `d` and `e` (`None`: nothing chosen, the default is followed; see `settings`).
    pub cover_alone: Option<bool>,
    pub count: Option<usize>,
    pub equal_height: Option<bool>,
    /// The zoom and place chosen by hand, if there were any.
    pub kept: Option<Relative>,
    /// The quarter turns given to the whole file, and those of single pages.
    pub file_turn: i32,
    pub turns: Vec<(usize, i32)>,
}

/// Everything remembered, and where it is kept (nowhere: nothing is read or written).
#[derive(Debug)]
pub struct State {
    file: Option<PathBuf>,
    /// The oldest first.
    books: Vec<(String, Book)>,
}

impl State {
    /// Nothing remembered and nothing kept.
    pub fn none() -> Self {
        Self {
            file: None,
            books: Vec::new(),
        }
    }

    /// What `dir` has kept (nothing, if there is nothing or it cannot be read); changes are written there.
    pub fn open(dir: PathBuf) -> Self {
        let file = dir.join("state");
        let mut state = Self::none();
        if let Ok(text) = std::fs::read_to_string(&file) {
            state.parse(&text);
        }
        state.file = Some(file);
        state
    }

    fn parse(&mut self, text: &str) {
        let mut lines = text.lines();
        if lines.next() != Some(HEADER) {
            return;
        }
        for line in lines {
            let fields: Vec<&str> = line.split('\t').collect();
            match fields.as_slice() {
                // (the first versions kept one number of pages and one height for all books on a line of their own: now they are settings)
                ["book", key, size, page, cover, file_turn, kept, turns] => {
                    if let Some(book) = parse_book(size, page, cover, file_turn, kept, turns, "-", "-") {
                        self.books.push(((*key).to_string(), book));
                    }
                }
                ["book", key, size, page, cover, file_turn, kept, turns, count, equal] => {
                    if let Some(book) = parse_book(size, page, cover, file_turn, kept, turns, count, equal) {
                        self.books.push(((*key).to_string(), book));
                    }
                }
                _ => {}
            }
        }
        let extra = self.books.len().saturating_sub(MAX_BOOKS);
        self.books.drain(..extra);
    }

    /// The book at `key` (the full path), if it is remembered and the file is still the size it was.
    pub fn get(&self, key: &str, size: u64) -> Option<&Book> {
        self.books
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, b)| b)
            .filter(|b| b.size == size)
    }

    /// Remembers the book as the most recent. A path with a tab or a line break in it cannot be kept in the file and is not.
    pub fn put(&mut self, key: &str, book: Book) {
        if key.contains(['\t', '\n', '\r']) {
            return;
        }
        self.books.retain(|(k, _)| k != key);
        self.books.push((key.to_string(), book));
        let extra = self.books.len().saturating_sub(MAX_BOOKS);
        self.books.drain(..extra);
    }

    fn text(&self) -> String {
        let mut out = format!("{HEADER}\n");
        let option = |v: Option<String>| v.unwrap_or_else(|| "-".to_string());
        for (key, b) in &self.books {
            let kept = b.kept.map_or_else(
                || "-".to_string(),
                |k| format!("{}:{}:{}", k.width_part, k.center.0, k.center.1),
            );
            let turns = if b.turns.is_empty() {
                "-".to_string()
            } else {
                b.turns
                    .iter()
                    .map(|(p, q)| format!("{p}:{q}"))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            out += &format!(
                "book\t{key}\t{}\t{}\t{}\t{}\t{kept}\t{turns}\t{}\t{}\n",
                b.size,
                b.page,
                option(b.cover_alone.map(|v| v.to_string())),
                b.file_turn,
                option(b.count.map(|v| v.to_string())),
                option(b.equal_height.map(|v| v.to_string())),
            );
        }
        out
    }

    /// Writes the file: a new one beside it first, then put in its place, so that a failure leaves the old one whole. A failure is
    /// not reported: remembering is a convenience and must not stop reading.
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
}

#[allow(clippy::too_many_arguments)]
fn parse_book(
    size: &str,
    page: &str,
    cover: &str,
    file_turn: &str,
    kept: &str,
    turns: &str,
    count: &str,
    equal: &str,
) -> Option<Book> {
    let kept = if kept == "-" {
        None
    } else {
        let mut parts = kept.split(':').map(str::parse::<f64>);
        let (w, x, y) = (parts.next()?.ok()?, parts.next()?.ok()?, parts.next()?.ok()?);
        (w.is_finite() && x.is_finite() && y.is_finite() && w > 0.0).then_some(Relative {
            width_part: w,
            center: (x, y),
        })
    };
    let turns = if turns == "-" {
        Vec::new()
    } else {
        turns
            .split(',')
            .filter_map(|t| {
                let (p, q) = t.split_once(':')?;
                Some((p.parse().ok()?, q.parse::<i32>().ok()?.rem_euclid(4)))
            })
            .collect()
    };
    Some(Book {
        size: size.parse().ok()?,
        page: page.parse().ok()?,
        cover_alone: option_of(cover, str::parse::<bool>)?,
        count: option_of(count, str::parse::<usize>)?.filter(|n| (1..=3).contains(n)),
        equal_height: option_of(equal, str::parse::<bool>)?,
        kept,
        file_turn: file_turn.parse::<i32>().ok()?.rem_euclid(4),
        turns,
    })
}

/// A field that is - (nothing chosen) or a value; None for a field that is neither (the line is not understood).
fn option_of<T, E>(field: &str, parse: impl Fn(&str) -> Result<T, E>) -> Option<Option<T>> {
    if field == "-" {
        Some(None)
    } else {
        parse(field).ok().map(Some)
    }
}

/// The folder for what the viewer remembers: `PEEKEL_STATE_DIR`, else the user's application data (`%APPDATA%\peekel` on Windows,
/// `$XDG_STATE_HOME/peekel` or `~/.local/state/peekel` elsewhere). None when there is no such place.
pub fn default_dir() -> Option<PathBuf> {
    let given = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(dir) = given("PEEKEL_STATE_DIR") {
        return Some(dir);
    }
    if cfg!(windows) {
        return given("APPDATA").map(|d| d.join("peekel"));
    }
    given("XDG_STATE_HOME")
        .or_else(|| given("HOME").map(|h| h.join(".local").join("state")))
        .map(|d| d.join("peekel"))
}

/// The key a file is remembered by: its full path.
pub fn key_of(path: &Path) -> String {
    std::path::absolute(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that does not find what it made should stop at once
mod tests {
    use super::*;

    fn book(page: usize) -> Book {
        Book {
            size: 1000,
            page,
            cover_alone: Some(false),
            count: Some(3),
            equal_height: Some(false),
            kept: Some(Relative {
                width_part: 1.25,
                center: (0.5, 0.25),
            }),
            file_turn: 1,
            turns: vec![(3, 2), (7, 1)],
        }
    }

    fn in_folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("peekel-state-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn what_is_put_comes_back_from_the_file() {
        let dir = in_folder("round");
        let mut state = State::open(dir.clone());
        state.put("/books/a.djvu", book(41));
        state.put(
            "/books/b.pdf",
            Book {
                kept: None,
                turns: Vec::new(),
                ..book(2)
            },
        );
        state.save();
        let again = State::open(dir.clone());
        assert_eq!(again.get("/books/a.djvu", 1000), Some(&book(41)));
        assert_eq!(again.get("/books/b.pdf", 1000).map(|b| b.page), Some(2));
        assert_eq!(
            again.get("/books/a.djvu", 999),
            None,
            "a file that has changed is another book"
        );
        assert_eq!(again.get("/books/none", 1000), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_book_put_again_replaces_the_old_and_the_oldest_are_forgotten() {
        let mut state = State::none();
        state.put("a", book(1));
        state.put("a", book(2));
        assert_eq!(state.get("a", 1000).map(|b| b.page), Some(2));
        for i in 0..MAX_BOOKS {
            state.put(&format!("book{i}"), book(i));
        }
        assert_eq!(state.get("a", 1000), None, "the oldest went");
        assert!(state.get("book0", 1000).is_some());
    }

    #[test]
    fn rubbish_in_the_file_is_passed_over() {
        let mut state = State::none();
        state.parse("peekel-state 1\nspread\t9\ttrue\nbook\tx\tnot a number\t1\ttrue\t0\t-\t-\nbook\ty\t5\t1\ttrue\t0\tnan:1:1\t-\nbook\tz\t5\t4\ttrue\t0\t-\t-\nbook\tshort\n\n");
        assert!(state.get("x", 5).is_none());
        assert!(
            state.get("y", 5).is_some_and(|b| b.kept.is_none()),
            "a zoom that is not a number is not kept"
        );
        assert_eq!(state.get("z", 5).map(|b| b.page), Some(4));
        let mut other = State::none();
        other.parse("something else\nbook\tz\t5\t4\ttrue\t0\t-\t-\n");
        assert!(other.get("z", 5).is_none(), "another kind of file");
    }

    #[test]
    fn the_older_lines_are_read_and_a_choice_that_was_not_made_stays_not_made() {
        let mut state = State::none();
        // the first form: eight fields, the cover always written, and one line of numbers for all books that is passed over now
        state.parse("peekel-state 1\nspread\t3\tfalse\nbook\told\t5\t4\ttrue\t0\t-\t-\nbook\tnew\t5\t4\t-\t0\t-\t-\t3\tfalse\nbook\tnone\t5\t4\t-\t0\t-\t-\t-\t-\nbook\tbad\t5\t4\t-\t0\t-\t-\t9\t-\n");
        let old = state.get("old", 5).expect("the old form");
        assert_eq!((old.cover_alone, old.count, old.equal_height), (Some(true), None, None));
        let new = state.get("new", 5).expect("the new form");
        assert_eq!(
            (new.cover_alone, new.count, new.equal_height),
            (None, Some(3), Some(false))
        );
        let none = state.get("none", 5).expect("nothing chosen");
        assert_eq!((none.cover_alone, none.count, none.equal_height), (None, None, None));
        // three pages at most: a number that is not 1 to 3 is not a choice
        assert_eq!(state.get("bad", 5).map(|b| b.count), Some(None));
        // what is written is read the same
        let text = state.text();
        let mut again = State::none();
        again.parse(&text);
        assert_eq!(again.get("new", 5), state.get("new", 5));
        assert_eq!(again.get("none", 5), state.get("none", 5));
    }
    #[test]
    fn a_path_with_a_tab_is_not_kept_and_a_state_with_no_folder_writes_nothing() {
        let mut state = State::none();
        state.put("a\tb", book(1));
        assert!(state.get("a\tb", 1000).is_none());
        state.save();
    }
}
