//! What a key press means to the viewer.

/// Something the viewer does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    ZoomIn,
    ZoomOut,
    /// The whole picture in the window.
    Fit,
    /// One pixel of the picture to one of the window.
    ActualSize,
    /// As wide as the window.
    FitWidth,
    /// This many steps right and down (negative: left and up); a step is an eighth of the window.
    Pan(i32, i32),
    Next,
    Previous,
    /// Page Down and Page Up: the next and the previous page of a document of many pages (a PDF), else the next and the
    /// previous file.
    PageDown,
    PageUp,
    /// The first and the last file of the folder (`g`, `G`).
    First,
    Last,
    /// Home and End: the first and the last spread of a book of many pages; in any other file the first and the last file.
    BookStart,
    BookEnd,
    /// The properties of the file (the EXIF of a photograph, the pages and author of a document), in place of the picture
    /// until the key is pressed again.
    Info,
    /// The path of the file, put on the terminal's clipboard.
    CopyPath,
    /// The magnet link of a `.torrent` copied to the clipboard (OSC 52).
    CopyMagnet,
    /// The page turned a quarter clockwise (`r`) or counter-clockwise (`R`): for a page that was scanned on its side.
    TurnRight,
    TurnLeft,
    /// Every page of the file turned a quarter clockwise (`a`) or counter-clockwise (`A`): for a book scanned on its side.
    TurnAllRight,
    TurnAllLeft,
    /// Asks for a word to look for in the text of the book (`/`); `n` and `N` go to the next and the previous page that has it.
    Search,
    /// The contents of a book (`c`): the list of its bookmarks to go to a chapter from.
    Contents,
    /// The settings of the viewer (`s`): a list to change them from.
    Settings,
    NextHit,
    PrevHit,
    /// How many pages of a book are shown side by side: 2, then 3, then 1, then 2 again.
    SpreadPages,
    /// Which page the pairs of a spread begin with: the first one alone (then 2-3, 4-5), or the first with the second (1-2, 3-4).
    SpreadShift,
    /// The pages of a spread made equally high, or each in its own size.
    EqualHeight,
    /// The text of a book whose text flows (FB2, EPUB) made larger (`f`) or smaller (`F`), the book laid out again.
    TextLarger,
    TextSmaller,
    /// A page of the book by its number, typed on the line at the bottom (`p`).
    GoToPage,
    /// The night look of books on or off (`b`): the pages dark and the text light.
    Night,
    /// The list of the keys, in place of the picture until any key is pressed.
    Help,
}

/// The letters of the Russian layout (ЙЦУКЕН) in the order of the keys of the Latin one that stand in the same places: q w e r t y u i
/// o p [ ] a s d f g h j k l ; ' z x c v b n m , . A terminal tells the letter that was typed and not the key that was pressed, so with
/// the Russian layout on, `q` arrives as `й`; the key is found from this table. (The key that is `/` in the Latin layout gives `.` in the
/// Russian one, which needs no table: `.` starts a search as `/` does.)
const RUSSIAN: &str = "йцукенгшщзхъфывапролджэячсмитьбю";
const LATIN: &str = "qwertyuiop[]asdfghjkl;'zxcvbnm,.";

/// The key of the Latin layout that the character `c` (a letter of the Russian layout, either case) sits on, as the character that key
/// gives; None for any other character.
pub fn latin_key(c: char) -> Option<char> {
    let lower = c.to_lowercase().next().unwrap_or(c);
    let at = RUSSIAN.chars().position(|r| r == lower)?;
    let key = LATIN.chars().nth(at)?;
    // a capital Russian letter is the capital key
    Some(if c != lower { key.to_ascii_uppercase() } else { key })
}

/// A key press as the viewer takes it: what to act on, and what a text field is given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pressed {
    /// The key to act on (the bytes [`action`] knows): the letter of the Latin layout that the key has, whatever layout is on.
    pub key: Vec<u8>,
    /// What typing gives: the character of the layout that is on (a Russian letter), or the key itself for Enter, Escape, an arrow.
    pub typed: Vec<u8>,
}

/// Turns what the terminal sends for a key into a [`Pressed`]:
/// - a record of Windows Terminal's key mode (`peekel_term::win32keys`): the key from the virtual key, the same with every layout;
/// - bytes that come one at a time (the terminal's usual way, which cuts a letter of another alphabet in pieces) are put together into
///   a character, and a letter of the Russian layout is turned into the key it sits on with a table ([`latin_key`]);
/// - an ASCII key or a sequence (an arrow) is as it is.
#[derive(Debug, Default)]
pub struct KeyDecoder {
    pending: Vec<u8>,
}

impl KeyDecoder {
    /// None while a character has not come whole yet, and for what makes no key (the release of a key, Shift held alone).
    pub fn feed(&mut self, key: &[u8]) -> Option<Pressed> {
        if key.starts_with(b"\x1b[")
            && let Some(record) = peekel_term::win32keys::parse(key)
        {
            self.pending.clear();
            return Some(Pressed {
                key: peekel_term::win32keys::key_bytes(&record)?,
                typed: peekel_term::win32keys::typed_bytes(&record)?,
            });
        }
        match key {
            [byte] if *byte >= 0x80 => {
                self.pending.push(*byte);
                match std::str::from_utf8(&self.pending) {
                    Ok(text) => {
                        let c = text.chars().next()?;
                        let typed = std::mem::take(&mut self.pending);
                        let key = latin_key(c).map_or_else(|| typed.clone(), |k| k.to_string().into_bytes());
                        Some(Pressed { key, typed })
                    }
                    Err(e) if e.error_len().is_none() && self.pending.len() < 4 => None,
                    Err(_) => {
                        self.pending.clear();
                        None
                    }
                }
            }
            _ => {
                self.pending.clear();
                Some(Pressed {
                    key: key.to_vec(),
                    typed: key.to_vec(),
                })
            }
        }
    }
}

/// The action for the bytes of one key, or None when the key means nothing here.
///
/// Arrows, Home, End and Page keys arrive as `ESC [ …` in most terminals and as `ESC O …` in some; Windows Terminal with
/// VT input sends the first.
pub fn action(key: &[u8]) -> Option<Action> {
    use Action::*;
    Some(match key {
        // Escape alone, Enter, Ctrl+C (a byte in raw mode), q
        b"\x1b" | b"\r" | b"\n" | b"\x03" | b"q" | b"Q" => Quit,
        b"+" | b"=" => ZoomIn,
        b"-" | b"_" => ZoomOut,
        b"0" => Fit,
        b"1" => ActualSize,
        b"w" | b"W" => FitWidth,
        b"\x1b[A" | b"\x1bOA" | b"k" => Pan(0, -1),
        b"\x1b[B" | b"\x1bOB" | b"j" => Pan(0, 1),
        b"\x1b[C" | b"\x1bOC" | b"l" => Pan(1, 0),
        b"\x1b[D" | b"\x1bOD" | b"h" => Pan(-1, 0),
        // Shift (the capital vi keys too) moves half a window, Ctrl a whole one
        b"\x1b[1;2A" | b"K" => Pan(0, -4),
        b"\x1b[1;2B" | b"J" => Pan(0, 4),
        b"\x1b[1;2C" | b"L" => Pan(4, 0),
        b"\x1b[1;2D" | b"H" => Pan(-4, 0),
        b"\x1b[1;5A" => Pan(0, -8),
        b"\x1b[1;5B" => Pan(0, 8),
        b"\x1b[1;5C" => Pan(8, 0),
        b"\x1b[1;5D" => Pan(-8, 0),
        b" " => Next,
        b"\x7f" | b"\x08" => Previous,
        b"\x1b[6~" => PageDown,
        b"\x1b[5~" => PageUp,
        b"i" | b"I" => Info,
        b"y" | b"Y" => CopyPath,
        b"m" | b"M" => CopyMagnet,
        b"r" => TurnRight,
        b"R" => TurnLeft,
        b"a" => TurnAllRight,
        b"A" => TurnAllLeft,
        // (the key of `/` gives `.` in the Russian layout)
        b"/" | b"." => Search,
        b"c" | b"C" => Contents,
        b"s" | b"S" => Settings,
        b"n" => NextHit,
        b"N" => PrevHit,
        b"d" | b"D" => SpreadPages,
        b"o" | b"O" => SpreadShift,
        b"e" | b"E" => EqualHeight,
        b"f" => TextLarger,
        b"F" => TextSmaller,
        b"p" | b"P" => GoToPage,
        b"b" | b"B" => Night,
        // F1 is `ESC O P` in xterm and most terminals, `ESC [ 11 ~` in some
        b"?" | b"\x1bOP" | b"\x1b[11~" => Help,
        b"\x1b[H" | b"\x1bOH" | b"\x1b[1~" => BookStart,
        b"\x1b[F" | b"\x1bOF" | b"\x1b[4~" => BookEnd,
        b"g" => First,
        b"G" => Last,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keys_that_close_it() {
        for key in [&b"\x1b"[..], b"\r", b"\n", b"\x03", b"q", b"Q"] {
            assert_eq!(action(key), Some(Action::Quit), "{key:?}");
        }
    }

    #[test]
    fn zoom_and_size_keys() {
        assert_eq!(action(b"+"), Some(Action::ZoomIn));
        assert_eq!(action(b"="), Some(Action::ZoomIn), "+ without Shift");
        assert_eq!(action(b"-"), Some(Action::ZoomOut));
        assert_eq!(action(b"0"), Some(Action::Fit));
        assert_eq!(action(b"1"), Some(Action::ActualSize));
        assert_eq!(action(b"w"), Some(Action::FitWidth));
    }

    #[test]
    fn arrows_in_both_dialects_and_the_vi_keys_move_the_picture() {
        assert_eq!(action(b"\x1b[A"), Some(Action::Pan(0, -1)));
        assert_eq!(action(b"\x1bOA"), Some(Action::Pan(0, -1)));
        assert_eq!(action(b"\x1b[B"), Some(Action::Pan(0, 1)));
        assert_eq!(action(b"\x1b[C"), Some(Action::Pan(1, 0)));
        assert_eq!(action(b"\x1b[D"), Some(Action::Pan(-1, 0)));
        assert_eq!(action(b"h"), Some(Action::Pan(-1, 0)));
        assert_eq!(action(b"j"), Some(Action::Pan(0, 1)));
        assert_eq!(action(b"k"), Some(Action::Pan(0, -1)));
        assert_eq!(action(b"l"), Some(Action::Pan(1, 0)));
    }

    #[test]
    fn shift_and_ctrl_make_the_steps_bigger() {
        assert_eq!(
            action(b"\x1b[1;2C"),
            Some(Action::Pan(4, 0)),
            "Shift+Right: half a window"
        );
        assert_eq!(action(b"J"), Some(Action::Pan(0, 4)));
        assert_eq!(
            action(b"\x1b[1;5D"),
            Some(Action::Pan(-8, 0)),
            "Ctrl+Left: a whole window"
        );
        assert_eq!(action(b"\x1b[1;5A"), Some(Action::Pan(0, -8)));
    }

    #[test]
    fn keys_that_go_through_the_folder() {
        assert_eq!(action(b" "), Some(Action::Next));
        assert_eq!(action(b"\x1b[6~"), Some(Action::PageDown), "Page Down");
        assert_eq!(action(b"\x7f"), Some(Action::Previous), "Backspace");
        assert_eq!(action(b"\x08"), Some(Action::Previous), "Backspace as Ctrl+H");
        assert_eq!(action(b"\x1b[5~"), Some(Action::PageUp), "Page Up");
        assert_eq!(action(b"\x1b[H"), Some(Action::BookStart), "Home");
        assert_eq!(action(b"\x1b[1~"), Some(Action::BookStart));
        assert_eq!(action(b"\x1b[F"), Some(Action::BookEnd), "End");
        assert_eq!(action(b"\x1b[4~"), Some(Action::BookEnd));
        assert_eq!(action(b"g"), Some(Action::First));
        assert_eq!(action(b"G"), Some(Action::Last));
    }

    #[test]
    fn y_copies_the_path() {
        assert_eq!(action(b"y"), Some(Action::CopyPath));
        assert_eq!(action(b"m"), Some(Action::CopyMagnet));
    }

    #[test]
    fn a_question_mark_and_f1_ask_for_the_help() {
        assert_eq!(action(b"?"), Some(Action::Help));
        assert_eq!(action(b"\x1bOP"), Some(Action::Help));
        assert_eq!(action(b"\x1b[11~"), Some(Action::Help));
    }

    #[test]
    fn other_keys_mean_nothing() {
        for key in [&b"x"[..], b"\x1b[2~", b"\x1bx", b"\xd0\xb9", b"X"] {
            assert_eq!(action(key), None, "{key:?}");
        }
    }

    #[test]
    fn r_turns_the_page_clockwise_and_capital_r_the_other_way() {
        assert_eq!(action(b"r"), Some(Action::TurnRight));
        assert_eq!(action(b"R"), Some(Action::TurnLeft));
        assert_eq!(action(b"a"), Some(Action::TurnAllRight));
        assert_eq!(action(b"A"), Some(Action::TurnAllLeft));
    }

    #[test]
    fn s_shows_the_settings_in_either_layout() {
        assert_eq!(action(b"s"), Some(Action::Settings));
        // the key of s has ы on it in the Russian layout
        let mut decoder = KeyDecoder::default();
        let y = "ы".as_bytes();
        assert_eq!(decoder.feed(&y[..1]), None);
        let key = decoder.feed(&y[1..]).map(|p| p.key).unwrap_or_default();
        assert_eq!(action(&key), Some(Action::Settings));
    }

    #[test]
    fn c_shows_the_contents_of_a_book() {
        assert_eq!(action(b"c"), Some(Action::Contents));
        assert_eq!(action(b"C"), Some(Action::Contents));
        // with the Russian layout on, the key of c has с on it
        let mut decoder = KeyDecoder::default();
        let s = "с".as_bytes();
        assert_eq!(decoder.feed(&s[..1]), None);
        let key = decoder.feed(&s[1..]).map(|p| p.key).unwrap_or_default();
        assert_eq!(action(&key), Some(Action::Contents));
    }

    #[test]
    fn slash_asks_for_a_word_and_n_goes_through_the_pages_that_have_it() {
        assert_eq!(action(b"/"), Some(Action::Search));
        assert_eq!(action(b"n"), Some(Action::NextHit));
        assert_eq!(action(b"N"), Some(Action::PrevHit));
        assert_eq!(action(b" "), Some(Action::Next));
        assert_eq!(action(b"\x7f"), Some(Action::Previous));
    }

    #[test]
    fn the_letters_of_the_russian_layout_are_the_keys_they_sit_on() {
        assert_eq!(latin_key('й'), Some('q'));
        assert_eq!(latin_key('Й'), Some('Q'));
        assert_eq!(latin_key('ф'), Some('a'));
        assert_eq!(latin_key('в'), Some('d'));
        assert_eq!(latin_key('р'), Some('h'));
        assert_eq!(latin_key('о'), Some('j'));
        assert_eq!(latin_key('л'), Some('k'));
        assert_eq!(latin_key('д'), Some('l'));
        assert_eq!(latin_key('т'), Some('n'));
        assert_eq!(latin_key('Т'), Some('N'));
        assert_eq!(latin_key('щ'), Some('o'));
        assert_eq!(latin_key('q'), None, "a Latin letter is itself");
        assert_eq!(latin_key('é'), None);
        assert_eq!(RUSSIAN.chars().count(), LATIN.chars().count());
    }

    fn pressed(key: &str, typed: &str) -> Option<Pressed> {
        Some(Pressed {
            key: key.as_bytes().to_vec(),
            typed: typed.as_bytes().to_vec(),
        })
    }

    #[test]
    fn a_letter_of_the_russian_layout_that_comes_a_byte_at_a_time_is_the_key_it_sits_on_and_is_typed_as_itself() {
        let mut decoder = KeyDecoder::default();
        let bytes = "й".as_bytes();
        assert_eq!(decoder.feed(&bytes[..1]), None, "half a letter");
        assert_eq!(decoder.feed(&bytes[1..]), pressed("q", "й"));
        // ASCII keys and sequences go through as they are, and the half letter that was left is dropped
        assert_eq!(decoder.feed(b"j"), pressed("j", "j"));
        assert_eq!(decoder.feed(b"\x1b[A"), pressed("\x1b[A", "\x1b[A"));
        assert_eq!(decoder.feed(&"Т".as_bytes()[..1]), None);
        assert_eq!(decoder.feed(b"x"), pressed("x", "x"));
        // a character with no key of ours stays what it is
        let e = "é".as_bytes();
        assert_eq!(decoder.feed(&e[..1]), None);
        assert_eq!(decoder.feed(&e[1..]), pressed("é", "é"));
    }

    #[test]
    fn a_record_of_windows_terminal_gives_the_key_from_the_virtual_key_and_the_text_from_the_layout() {
        let mut decoder = KeyDecoder::default();
        // the D key with the Russian layout: down, then up
        assert_eq!(decoder.feed(b"\x1b[68;32;1074;1;32;1_"), pressed("d", "в"));
        assert_eq!(decoder.feed(b"\x1b[68;32;1074;0;32;1_"), None);
        // Shift held alone makes no key; Ctrl+C is the control character whatever the layout
        assert_eq!(decoder.feed(b"\x1b[16;42;0;1;48;1_"), None);
        assert_eq!(decoder.feed(b"\x1b[67;46;3;1;40;1_"), pressed("\x03", "\x03"));
        // a sequence that is no record is a sequence
        assert_eq!(decoder.feed(b"\x1b[6~"), pressed("\x1b[6~", "\x1b[6~"));
    }

    #[test]
    fn d_o_and_e_set_up_the_spread() {
        assert_eq!(action(b"d"), Some(Action::SpreadPages));
        assert_eq!(action(b"o"), Some(Action::SpreadShift));
        assert_eq!(action(b"e"), Some(Action::EqualHeight));
    }

    #[test]
    fn f_makes_the_text_of_a_book_larger_and_capital_f_smaller_in_either_layout() {
        assert_eq!(action(b"f"), Some(Action::TextLarger));
        assert_eq!(action(b"F"), Some(Action::TextSmaller));
        // the key of f has а on it in the Russian layout
        assert_eq!(latin_key('а'), Some('f'));
        assert_eq!(latin_key('А'), Some('F'));
    }

    #[test]
    fn p_asks_for_a_page_in_either_layout() {
        assert_eq!(action(b"p"), Some(Action::GoToPage));
        assert_eq!(action(b"P"), Some(Action::GoToPage));
        // the key of p has з on it in the Russian layout
        assert_eq!(latin_key('з'), Some('p'));
    }
}
