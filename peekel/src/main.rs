// The binary talks to the terminal; the print lints are for the library crates.
#![allow(clippy::print_stdout, clippy::print_stderr)]

mod files;
mod install;
mod keys;
mod layout;
mod settings;
mod state;

use std::ffi::OsString;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use peekel_core::decode::{DecodeError, decode_page, mark_words, page_at, page_texts, place_of, text_flows};
use peekel_core::photo::{Steady, quantize};
use peekel_core::quantize::Indexed;
use peekel_core::sixel;
// the spread and the search live in the core, shared with Peekel TV
use peekel_core::view::{Image, Relative, Size, View, ZOOM_STEP, render, render_sharp, side_by_side, turned_rgba};
use peekel_core::{find, spread};
use peekel_term::reply::{Event, Parser};
use peekel_term::{RawMode, WindowSize, input, window_size};

use files::Listing;
use keys::{Action, KeyDecoder, action};
use layout::{Cell, DEFAULT_CELL, cell_from_window, for_told_cells, origin, real_cell, room};
use settings::{BookChoice, Scans, Scope, Settings};
use spread::Spread;
use state::{Book, State};

/// `text` with its control characters (a file name may hold an escape sequence, and the terminal would obey it) made `?`.
fn plain(text: &str) -> String {
    text.chars().map(|c| if c.is_control() { '?' } else { c }).collect()
}

const USAGE: &str = "usage: peekel [options] <file>

Shows a picture, an SVG, the start of an Excel, PowerPoint or OpenDocument file, and on request (--cards) a card for any other file; reads PDF,
DjVu, FB2 and EPUB books (and HTML pages and Word documents) page by page. In a terminal that displays Sixel graphics (Windows Terminal 1.22 or
later, xterm, mlterm, foot and others), with the other files of its folder.

keys:
  + -               zoom in, out         0   the whole picture      1   one pixel to one      w   as wide as the window
  arrows, h j k l   move the picture     Space, Backspace   the next, the previous file     g, G   the first, the last file
  Home, End         the beginning, the end of a book (in other files: the first, the last file)
  Page Down, Up     the next, the previous spread of a PDF, DjVu, FB2 or EPUB book (two pages side by side; in other files: the next, the previous file)
  p                 go to a page of the book by its number (Enter goes, Esc gives it up)
  d                 pages side by side: 2, 3, 1     o   shift the pairs (the first page alone or with the second)     e   equal height or not
  i                 the properties of the file (a photograph's EXIF, a document's author and pages); again: back
  Shift, Ctrl+arrows   a bigger step: half a window, a whole one (H J K L: half)
  mouse             wheel: zoom; drag: move the picture; a click in the left or right fifth: the previous or the next file
  y                 copy the file's path to the terminal's clipboard (OSC 52; some terminals refuse)
  m                 copy the magnet link of a .torrent to the clipboard (the same way)
  ?, F1             this list of keys (any key takes it away)
  Esc, q, Enter     close

options:
  --any-key     any key closes it (for a quick look)
  --no-status   no line of information at the bottom
  --no-state    do not remember, or use, the page, the zoom and the turns of each book between runs (the file is
                %APPDATA%\\peekel\\state, or $XDG_STATE_HOME/peekel/state, or PEEKEL_STATE_DIR/state)
  --force       draw even if the terminal does not say it shows Sixel (some do and do not say; tmux and screen do not)
  --cards       a file that is no picture (a font, an archive, a program) is shown as a card: its kind, its size and dates, and
                for a font a sample of text; without this such a file is skipped quietly (also PEEKEL_CARDS=1, or the
                settings: s)
  --timing      after the picture is closed, print how long each stage took
  --masks far   print the mask of the files it reads, for Far Manager's file associations (F3)
  --masks mc    print the section of Midnight Commander's mc.ext.ini that opens them with peekel

installing (each step asks first; --yes answers yes to all):
  --install             copy itself to %LOCALAPPDATA%\\Programs\\peekel (Windows; its folder put on PATH) or ~/.local/bin, the
                        fonts beside it to its own folder (Linux), and set up the file managers it finds
  --setup far [Far.exe] Far Manager's F3 opens these files in peekel (Far closed; its associations backed up first)
  --setup mc            Midnight Commander's F3 and Enter open them (mc.ext.ini backed up first)
  --uninstall           take all of that away again";

/// Device attributes (a 4 in the answer means Sixel), the cell's size in pixels and the terminal's background colour.
const QUESTIONS: &str = "\x1b[?9001$p\x1b[c\x1b[16t\x1b]11;?\x1b\\";

/// "Are you in working order?": answered `CSI 0 n` once the terminal has dealt with everything sent before it.
const STATUS_QUESTION: &str = "\x1b[5n";

/// The alternate screen, so that whatever was on the screen comes back; the cursor hidden; the screen cleared; the
/// mouse reported (presses, and moves while a button is held, in the form that has no limit on the columns).
const ENTER_SCREEN: &str = "\x1b[?1049h\x1b[?25l\x1b[2J\x1b[?1000h\x1b[?1002h\x1b[?1006h";
const LEAVE_SCREEN: &str = "\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?25h\x1b[?1049l";

/// Synchronised output (private mode 2026): what is written between the two is shown at once, not piece by piece.
const BEGIN_FRAME: &str = "\x1b[?2026h";
const END_FRAME: &str = "\x1b[?2026l";

/// How long the terminal is given to answer; once it has said what it is, a shorter wait for the cell's size.
const ANSWER_WAIT: Duration = Duration::from_millis(1000);
const CELL_WAIT: Duration = Duration::from_millis(300);

/// How often the window's size is looked at while waiting for a key, and how long a lone Escape byte waits for more.
const TICK: Duration = Duration::from_millis(50);
/// A picture that has been moved or zoomed is drawn in full quality after this long without another move.
const SETTLE: Duration = Duration::from_millis(100);
/// The strength of the ordered dithering, in levels of a channel, in the frames drawn while the picture is being moved
/// (`PEEKEL_MOVE_DITHER`; `PEEKEL_MOVE_COARSE=0` draws every frame in full).
const MOVE_DITHER: i32 = 6;

/// What the line at the bottom says when there is nothing else to say.
const HINT: &str =
    "? help  + - zoom  0 fit  1 1:1  w width  arrows move  Space/Backspace file  PgUp/PgDn page  i info  q close";

/// The list of keys shown on `?` (the key, what it does).
const HELP_ROWS: &[(&str, &str)] = &[
    (
        "+  -",
        "zoom in, out (the zoom and place stay on the next pages of the file)",
    ),
    ("0", "the whole picture (and the next pages whole)"),
    ("1", "one pixel to one"),
    ("w", "as wide as the window"),
    ("arrows, h j k l", "move the picture"),
    (
        "Shift, Ctrl + arrows",
        "move it half a window, a whole one (H J K L: half)",
    ),
    ("Space, Backspace", "the next, the previous file"),
    (
        "Home, End",
        "the beginning, the end of a book (other files: the first, the last file)",
    ),
    ("g, G", "the first, the last file of the folder"),
    ("Page Down, Page Up", "the next, the previous spread of a book"),
    ("p", "go to a page of the book by its number"),
    ("mouse wheel", "zoom in, out"),
    ("drag", "move the picture"),
    ("click at the left, right edge", "the previous, the next file"),
    ("i", "the properties of the file; again: back"),
    (
        "c",
        "the contents of a book (the bookmarks of a DjVu book or a PDF); Enter goes to the chapter",
    ),
    (
        "s",
        "the settings of the viewer: pages side by side for each book or for all, and the defaults",
    ),
    ("d", "pages of a book side by side: 2, 3, 1"),
    ("o", "shift the pairs: the first page alone, or with the second"),
    ("e", "pages of a spread equally high, or each in its own size"),
    (
        "f, F",
        "the text of an FB2, EPUB or Word book larger, smaller (laid out again)",
    ),
    (
        "t",
        "paragraphs an EPUB, HTML or Word book sets left justified too, or left as the book sets them",
    ),
    (
        "b",
        "night: the pages of books dark and the text light; again: as they are",
    ),
    (
        "v",
        "the pages of DjVu books drawn for the window, or whole at the size of the scan (to compare)",
    ),
    (
        "r, R",
        "turn the page a quarter clockwise, the other way (for a page scanned on its side)",
    ),
    ("y", "copy the file's path to the clipboard"),
    ("m", "copy the magnet link of a .torrent to the clipboard"),
    ("?, F1", "this list"),
    ("Esc, q, Enter", "close"),
];

/// A setting on the screen of the settings (`s`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Setting {
    TextSize,
    Justify,
    MarkParagraphs,
    ChosenSize,
    CodeBackdrop,
    Night,
    Scope,
    Pages,
    EqualHeight,
    CoverAlone,
    Scans,
    ScansAsDjvulibre,
    Browse,
}

/// A line of the screen of the settings: the name of a part, or a setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuLine {
    Part(&'static str),
    Row(Setting),
}

/// The lines of the screen of the settings, in their parts. Every setting is here once (a test says so): the keys and the mouse go
/// through the rows of this list, so a setting left out could not be reached (the night row once stood below the last row the keys
/// reached).
const MENU: [MenuLine; 17] = [
    MenuLine::Part("Books whose text flows: FB2, EPUB, HTML, Word"),
    MenuLine::Row(Setting::TextSize),
    MenuLine::Row(Setting::Justify),
    MenuLine::Row(Setting::MarkParagraphs),
    MenuLine::Row(Setting::ChosenSize),
    MenuLine::Row(Setting::CodeBackdrop),
    MenuLine::Part("Pages of every book"),
    MenuLine::Row(Setting::Night),
    MenuLine::Row(Setting::Scope),
    MenuLine::Row(Setting::Pages),
    MenuLine::Row(Setting::EqualHeight),
    MenuLine::Row(Setting::CoverAlone),
    MenuLine::Part("DjVu"),
    MenuLine::Row(Setting::Scans),
    MenuLine::Row(Setting::ScansAsDjvulibre),
    MenuLine::Part("Files"),
    MenuLine::Row(Setting::Browse),
];

/// Whether line `i` of `MENU` is a setting (not the name of a part).
fn menu_is_row(i: usize) -> bool {
    matches!(MENU.get(i), Some(MenuLine::Row(_)))
}

/// The line of `MENU` of the next setting below `from` (`down`) or above it, past the names of the parts; `from` when there is none.
fn menu_next(from: usize, down: bool) -> usize {
    if down {
        (from + 1..MENU.len()).find(|&i| menu_is_row(i)).unwrap_or(from)
    } else {
        (0..from.min(MENU.len()))
            .rev()
            .find(|&i| menu_is_row(i))
            .unwrap_or(from)
    }
}

/// The line of `MENU` of the first setting (`first`) or the last.
fn menu_end(first: bool) -> usize {
    if first {
        (0..MENU.len()).find(|&i| menu_is_row(i)).unwrap_or(0)
    } else {
        (0..MENU.len()).rev().find(|&i| menu_is_row(i)).unwrap_or(0)
    }
}

/// How many lines the screen of the settings has: those of `MENU`, and the one that says where the settings are saved.
fn menu_total() -> usize {
    MENU.len() + 1
}

/// What a setting does, in one line: it stands under the title of the screen while the setting is chosen.
fn setting_hint(setting: Setting) -> &'static str {
    match setting {
        Setting::TextSize => "The size of the text, in per cent of the book's own; the book is laid out again",
        Setting::Justify => {
            "Yes: paragraphs a book sets left are justified and hyphenated too; centred and right stay so"
        }
        Setting::MarkParagraphs => {
            "Yes: a paragraph with no indent and no space above it gets an indent; a table gets room below it"
        }
        Setting::ChosenSize => {
            "Yes: the size most of the text is set in is the size chosen, headings in proportion; no: the book's sizes"
        }
        Setting::CodeBackdrop => "Yes: blocks of program code are drawn on a light shade",
        Setting::Night => {
            "Yes: the pages dark and the text light, for the dark; photographs and covers stay as they are"
        }
        Setting::Scope => {
            "For each book: d and e are remembered for the book; for all books: they change the defaults below"
        }
        Setting::Pages => "How many pages stand side by side in a book given no choice of its own (d for a book)",
        Setting::EqualHeight => "Yes: the pages of a spread made as high as the tallest one (e for a book)",
        Setting::CoverAlone => "Yes: the first page alone, then 2-3, 4-5; no: 1-2, 3-4 (o for a book)",
        Setting::Scans => {
            "window: for the window, the least memory; finer: twice the pixels, shrunk; whole: the scan's own size"
        }
        Setting::ScansAsDjvulibre => {
            "Yes: a page drawn at half its scan or smaller made as DjVuLibre makes it; no: ours, sharper letters"
        }
        Setting::Browse => "Which files of the folder Space and Backspace go through",
    }
}

/// The screen of the settings, while it is shown: the line of `MENU` chosen (always a setting) and the first line on the screen.
struct Menu {
    selected: usize,
    first: usize,
}

/// What is drawn of the list of contents (see `Viewer::toc_list`).
struct TocList {
    subtitle: String,
    lines: Vec<peekel_core::card::ListLine>,
    marked: Option<usize>,
    first: usize,
    total: usize,
}

type RowsFor = ((usize, usize), (usize, usize), [u8; 3]);

/// Whether the terminal is Windows Terminal. `WT_SESSION` is set in it, but it is set as well in a terminal started from a shell that
/// runs in it (xterm and foot started from a WSL shell were met), so the terminal is asked too: only Windows Terminal knows the key
/// mode 9001. Set once, when the terminal has answered (`show`); not known before.
static WINDOWS_TERMINAL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Whether the terminal moves the Sixel pictures with the text when it scrolls a region: seen in Windows Terminal (a test of
/// CSI S, T, M and L, 3 November 2026), not known of the others, so they get their rows written whole.
fn terminal_moves_pictures() -> bool {
    WINDOWS_TERMINAL.get().copied().unwrap_or(false)
}

/// The most bytes of one Sixel string that a terminal not known to take longer ones is given: xterm drops a string of 0.67 MB and takes
/// one of 0.57 MB (3 November 2026), and a page at the size of a window is 0.7 to 1.6 MB. A longer picture is sent as strips
/// (`sixel::encode_limited`). Windows Terminal has no limit that was met (strings of 3 MB were taken), so it gets one string, as
/// before. `PEEKEL_SIXEL_LIMIT=bytes` gives it by hand, 0 for no limit.
const SIXEL_LIMIT: usize = 400_000;

fn sixel_limit() -> Option<usize> {
    match std::env::var("PEEKEL_SIXEL_LIMIT")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
    {
        Some(0) => None,
        Some(bytes) => Some(bytes),
        None => (!terminal_moves_pictures()).then_some(SIXEL_LIMIT),
    }
}

/// A picture that `make` makes, `size` pixels, as a Sixel stream on the `background`; the time of the four stages (making, scaling,
/// quantizing, encoding) in milliseconds is added to `times`. Nothing if the picture could not be made.
fn strip_sixel(
    make: impl FnOnce() -> Option<Image>,
    size: (usize, usize),
    background: [u8; 3],
    times: &mut [u128; 4],
) -> Option<String> {
    let mut lap = Instant::now();
    let mut image = make()?;
    times[0] += lap.elapsed().as_millis();
    lap = Instant::now();
    let shown = image.size();
    let mut frame = render(&mut image, &View::new(shown), size)?;
    peekel_core::decode::composite_over(&mut frame.picture.argb, background);
    times[1] += lap.elapsed().as_millis();
    lap = Instant::now();
    let width = frame.picture.width;
    let rows = frame.picture.argb.len().checked_div(width).unwrap_or(0);
    let indexed = quantize(&frame.picture.argb, width);
    times[2] += lap.elapsed().as_millis();
    lap = Instant::now();
    let [r, g, b] = background;
    let packed = u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b);
    let stream = sixel::encode_indexed(&indexed.index, &indexed.palette, width, 0, rows, packed);
    times[3] += lap.elapsed().as_millis();
    Some(stream)
}

/// The Sixel of the rows of a list that were made, and what they were made for (the room, the cell and the background): a row is
/// whole cells high, so what is made once stands at any line, and a list that scrolls is rows put on the screen again.
#[derive(Default)]
struct RowCache {
    /// (the room in pixels, the cell, the background)
    made_for: Option<RowsFor>,
    /// By the text, the right-hand text, the depth, whether the row is the chosen one, and the width of the right-hand column.
    rows: std::collections::HashMap<(String, String, usize, bool, usize), String>,
}

/// A search in a book: the word, the pages (from 0) that have it, and which of those was shown last.
#[derive(Debug)]
struct Find {
    needle: String,
    hits: Vec<usize>,
    at: Option<usize>,
}

/// The contents of a book as a list on the screen: the lines and the one chosen.
#[derive(Debug)]
struct Toc {
    items: Vec<peekel_core::djvu::ContentsItem>,
    selected: usize,
    /// The first line that is on the screen.
    first: usize,
}

/// The line of the contents that is the chapter of page `page` (counted from 0): the one with the greatest page that is not past it, the
/// last of them when several lines have the same page; the first line when no line is as early. (Not the last line whose page is not
/// past it: a book may end its contents with a line that leads back to an early page, and that line would be chosen for every page.)
/// The page a remembered book is opened on: the page it was left on, or for a book whose text flows left at another size of the text,
/// the page its place is on now (the book is laid out for that).
fn page_of(path: &Path, book: &Book) -> usize {
    match book.place {
        Some((place, percent)) if percent != peekel_core::book::text_size() => {
            page_at(path, place).ok().flatten().unwrap_or(book.page)
        }
        _ => book.page,
    }
}

fn chapter_at(items: &[peekel_core::djvu::ContentsItem], page: usize) -> usize {
    items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| item.page.filter(|&p| p <= page).map(|p| (p, i)))
        .max()
        .map_or(0, |(_, i)| i)
}
/// The most characters of a word to look for.
const MAX_NEEDLE: usize = 120;
/// The most digits of a page's number (`p`).
const MAX_PAGE_DIGITS: usize = 7;

/// A drag with the left button.
#[derive(Debug, Clone, Copy)]
struct Drag {
    column: u32,
    row: u32,
    moved: bool,
}

#[derive(Debug, Clone, Copy)]
struct Options {
    timing: bool,
    any_key: bool,
    status: bool,
    force: bool,
    /// What is remembered about each book between runs (off with `--no-state`).
    remember: bool,
    /// A file that is no picture gets a card (off: it is reported and skipped).
    cards: bool,
}

/// Which files of the folder Space and Backspace go through: every file with `--cards` (`PEEKEL_CARDS`), else as the settings say.
fn listing(options: &Options, settings: &Settings) -> Listing {
    if options.cards { Listing::Every } else { settings.browse }
}

fn main() -> ExitCode {
    let mut options = Options {
        timing: false,
        any_key: false,
        status: true,
        force: false,
        remember: true,
        cards: std::env::var("PEEKEL_CARDS").is_ok_and(|v| !v.is_empty() && v != "0"),
    };
    let mut path: Option<OsString> = None;
    let mut args = std::env::args_os().skip(1);
    let mut action: Option<install::Action> = None;
    let mut yes = false;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--install") => action = Some(install::Action::Install),
            Some("--uninstall") => action = Some(install::Action::Uninstall),
            Some("--yes") => yes = true,
            Some("--setup") => {
                action = match args.next().as_deref().and_then(|a| a.to_str()) {
                    Some("mc") => Some(install::Action::SetupMc),
                    Some("far") => Some(install::Action::SetupFar(None)),
                    _ => {
                        eprintln!("peekel: --setup mc, or --setup far [path of Far.exe]");
                        return ExitCode::from(2);
                    }
                };
            }
            Some("--masks") => {
                return match args.next().as_deref().and_then(|a| a.to_str()) {
                    Some("far") => {
                        println!("{}", files::far_mask());
                        ExitCode::SUCCESS
                    }
                    Some("mc") => {
                        print!("{}", files::mc_section());
                        ExitCode::SUCCESS
                    }
                    _ => {
                        eprintln!("peekel: --masks far, or --masks mc");
                        ExitCode::from(2)
                    }
                };
            }
            Some("-h" | "--help") => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            Some("-V" | "--version") => {
                println!("peekel {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            Some("--timing") => options.timing = true,
            Some("--any-key") => options.any_key = true,
            Some("--no-status") => options.status = false,
            Some("--force") => options.force = true,
            Some("--cards") => options.cards = true,
            Some("--no-state") => options.remember = false,
            _ => {
                if let Some(install::Action::SetupFar(far @ None)) = &mut action {
                    *far = Some(PathBuf::from(arg));
                } else if path.replace(arg).is_some() {
                    eprintln!("peekel: one picture at a time\n{USAGE}");
                    return ExitCode::from(2);
                }
            }
        }
    }
    if let Some(action) = action {
        return match install::run(&action, &install::Asker { yes }) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("peekel: {message}");
                ExitCode::FAILURE
            }
        };
    }
    let Some(path) = path else {
        // nothing to show is no mistake: the same as --help (winget's check runs the program so; docs/internal/winget.md)
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    };
    if !(io::stdin().is_terminal() && io::stdout().is_terminal()) {
        eprintln!("peekel: needs a terminal on standard input and output");
        return ExitCode::from(2);
    }
    match show(Path::new(&path), options) {
        Ok(report) => {
            if options.timing {
                eprintln!("peekel: {report}");
            }
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("peekel: {message}");
            ExitCode::FAILURE
        }
    }
}

/// What the terminal said about itself.
#[derive(Debug, Default)]
struct Probe {
    sixel: Option<bool>,
    /// The numbers of the terminal's answer to "what are you?" (device attributes): 4 among them means Sixel.
    attributes: Vec<u32>,
    cell: Option<Cell>,
    /// The terminal's background colour, when it told.
    background: Option<[u8; 3]>,
    /// The terminal knows Windows Terminal's key mode (9001): it tells the key that was pressed, not only the letter that came of it.
    key_records: bool,
}

/// Reads the answers to [`QUESTIONS`]; a terminal that does not know a question ignores it, so the wait is bounded.
fn probe(keys: &Receiver<(u8, Instant)>, parser: &mut Parser) -> Probe {
    let mut found = Probe::default();
    let mut deadline = Instant::now() + ANSWER_WAIT;
    while found.sixel.is_none() || found.cell.is_none() || found.background.is_none() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let Ok((byte, _)) = keys.recv_timeout(left) else { break };
        match parser.push(byte) {
            Some(Event::DeviceAttributes(attributes)) => {
                found.sixel = Some(attributes.contains(&4));
                found.attributes = attributes;
                deadline = deadline.min(Instant::now() + CELL_WAIT);
            }
            Some(Event::CellSize { width, height }) => found.cell = Some(Cell { width, height }),
            Some(Event::Background(rgb)) => found.background = Some(rgb),
            // set, reset, or fixed on or off: the mode is known (0 is "not known")
            Some(Event::ModeReport { mode: 9001, state }) => found.key_records = state != 0,
            _ => {}
        }
    }
    found
}

/// How long each stage of showing the first picture took (`--timing`).
#[derive(Debug, Default)]
struct Report {
    probe: Duration,
    decode: Duration,
    quantize: Duration,
    encode: Duration,
    paint: Duration,
    /// From the start of writing the picture to the terminal's answer to "are you in working order?", which it gives
    /// only after it has dealt with all the picture's bytes; None when it had not answered before the key was pressed.
    processed: Option<Duration>,
    size: (usize, usize),
    stream_bytes: usize,
    /// For each redraw after the first (a zoom, a move, another picture's frame): the time to make the frame (scaling,
    /// palette, encoding) and the time to write it.
    /// The last is whether the frame was a coarse one (drawn while the picture was being moved).
    redraws: Vec<(Duration, Duration, bool)>,
    /// The cell the terminal tells, the cell the picture was laid out in, and what was measured to find it (`layout::cell_report`).
    cell: String,
}

/// The median and the largest of some durations, in milliseconds.
fn median_and_max(times: &[Duration]) -> (f64, f64) {
    let mut ms: Vec<f64> = times.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
    ms.sort_by(f64::total_cmp);
    (
        ms.get(ms.len() / 2).copied().unwrap_or(0.0),
        ms.last().copied().unwrap_or(0.0),
    )
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        write!(
            f,
            "{}x{} px, {:.1} MB of Sixel; asking the terminal {:.0} ms, decode and scale {:.0} ms, palette and dither {:.0} ms, \
             Sixel encoding {:.0} ms, writing to the terminal {:.0} ms",
            self.size.0,
            self.size.1,
            self.stream_bytes as f64 / 1e6,
            ms(self.probe),
            ms(self.decode),
            ms(self.quantize),
            ms(self.encode),
            ms(self.paint),
        )?;
        write!(f, "; {}", self.cell)?;
        match self.processed {
            Some(d) => write!(
                f,
                "; the terminal had dealt with it {:.0} ms after the writing began (so its own work was about {:.0} ms \
                 after the last byte was handed over; drawing on the screen may take a little longer)",
                ms(d),
                ms(d.saturating_sub(self.paint)),
            )?,
            None => write!(
                f,
                "; the terminal did not answer the status question before the key was pressed"
            )?,
        }
        for (name, coarse) in [("full", false), ("coarse", true)] {
            let of_kind: Vec<&(Duration, Duration, bool)> = self.redraws.iter().filter(|r| r.2 == coarse).collect();
            if of_kind.is_empty() {
                continue;
            }
            let made: Vec<Duration> = of_kind.iter().map(|r| r.0).collect();
            let written: Vec<Duration> = of_kind.iter().map(|r| r.1).collect();
            let (made_median, made_max) = median_and_max(&made);
            let (written_median, written_max) = median_and_max(&written);
            write!(
                f,
                "; {} {name} redraws: making a frame median {made_median:.0} ms, slowest {made_max:.0} ms; writing it \
                 median {written_median:.0} ms, slowest {written_max:.0} ms",
                of_kind.len(),
            )?;
        }
        Ok(())
    }
}

/// A picture ready to be painted: its Sixel stream, its size in pixels, the scale it is drawn at.
struct Rendered {
    stream: String,
    size: (usize, usize),
    scale: f64,
    scaling: Duration,
    quantizing: Duration,
    encoding: Duration,
    /// A coarse frame, rougher than the full one would be: it is drawn in full when the picture has stopped.
    rough: bool,
}

/// What a frame is made for: the window, its cells and the terminal's background.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Canvas {
    window: WindowSize,
    cell: Cell,
    told: Cell,
    background: [u8; 3],
}

/// The window's pixels of `image` for `view`, as Sixel, its palette made by `quantizer` (which says too whether the frame is rough).
/// With `sharp`, a picture drawn from an SVG and shown enlarged has the part shown drawn again from its shapes (`render_sharp`): not for
/// the quick frames while it is being moved, which take the picture as it is.
fn picture_frame(
    image: &mut Image,
    view: &View,
    canvas: Canvas,
    sharp: bool,
    quantizer: impl FnOnce(&[u32], usize) -> (Indexed, bool),
) -> Result<Rendered, String> {
    let room = room(canvas.window, canvas.cell);
    let scale = view.scale(image.size(), room);
    let started = Instant::now();
    let frame = if sharp {
        render_sharp(image, view, room)
    } else {
        render(image, view, room)
    };
    let mut frame = frame.ok_or_else(|| "the window is too small".to_string())?;
    // what is clear or half clear is laid over the terminal's own background (Sixel has no half-clear pixel)
    peekel_core::decode::composite_over(&mut frame.picture.argb, canvas.background);
    // a terminal whose real cell is another shape than the one it tells stretches the picture over the real cells: the picture
    // was fitted in real pixels, and what is sent is made for the told cell (`layout::real_cell`)
    let for_terminal = for_told_cells(&frame.picture.argb, frame.picture.width, canvas.cell, canvas.told);
    let (argb, width): (&[u32], usize) = match &for_terminal {
        Some((argb, width)) => (argb, *width),
        None => (&frame.picture.argb, frame.picture.width),
    };
    let scaled = Instant::now();
    let (indexed, rough) = quantizer(argb, width);
    let quantized = Instant::now();
    // transparent pixels are painted in the terminal's background: whatever was under the picture is covered
    let [r, g, b] = canvas.background;
    let background = u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b);
    let rows = argb.len().checked_div(width).unwrap_or(0);
    let stream = match sixel_limit() {
        None => sixel::encode_indexed(&indexed.index, &indexed.palette, width, 0, rows, background),
        Some(limit) => {
            // strips of whole cells, each put at its own cell (the first is put by `paint`)
            let (row, column) = origin(canvas.window, canvas.cell, (frame.picture.width, frame.picture.height));
            let unit = canvas.told.height as usize;
            sixel::encode_limited(&indexed.index, &indexed.palette, width, rows, background, unit, limit)
                .into_iter()
                .map(|(top, strip)| match top {
                    0 => strip,
                    _ => format!("\x1b[{};{column}H{strip}", row + top / unit.max(1)),
                })
                .collect()
        }
    };
    Ok(Rendered {
        stream,
        size: (frame.picture.width, frame.picture.height),
        scale,
        scaling: scaled - started,
        quantizing: quantized - scaled,
        encoding: quantized.elapsed(),
        rough,
    })
}

/// A frame made on the thread that read the picture, for the canvas and the view it was made for.
struct Ready {
    canvas: Canvas,
    view: View,
    rendered: Rendered,
}

/// What is on the screen: the picture's size and the cell it starts at.
#[derive(Debug, PartialEq, Eq)]
struct Drawn {
    size: (usize, usize),
    origin: (usize, usize),
}

/// A picture and where it is looked at.
struct Viewer {
    files: Vec<PathBuf>,
    index: usize,
    image: Image,
    /// The page of the file that `image` is (counted from 0) and how many pages the file has.
    page: usize,
    pages: usize,
    /// How many pages of the file `image` has side by side (it begins with `page`), and how a spread is made up.
    shown: usize,
    spread: Spread,
    /// The properties of the file made into a picture the size of the window (when they were asked for), and whether they
    /// are on the screen in place of the picture.
    info: Option<Image>,
    show_info: bool,
    /// The list of the keys is on the screen (any key takes it away).
    help: bool,
    /// The left button is held: where it went down or was last seen, and whether the picture has been dragged since.
    drag: Option<Drag>,
    /// The file is shown as a card (it is no picture): the card stays on the screen, and `i` and the zoom keys mean nothing.
    card: bool,
    view: View,
    window: WindowSize,
    /// The cell the layout is worked out in: the terminal's real one (`layout::real_cell`), which is `told` unless the real one is another shape.
    cell: Cell,
    /// The cell the terminal tells; a picture is made for it (`layout::for_told_cells`) when it is not `cell`.
    told: Cell,
    /// The terminal's background colour (black when it did not tell): transparent pixels are painted with it, and a
    /// card's text is chosen to be read on it.
    background: [u8; 3],
    options: Options,
    /// Something to say in place of the hint (a picture that could not be read).
    message: Option<String>,
    drawn: Option<Drawn>,
    /// The neighbouring picture being read while this one is looked at.
    preload: Option<Preload>,
    /// The frame of the picture just taken in, made on its thread with the read (whole, in the middle of the window): the key
    /// that asked for it waits only for the writing. Used by the next frame if the window and the view are still those.
    ready: Option<Ready>,
    /// The picture left last, with its halved copies: a step back takes it in place of reading the file again. One, like the read
    /// ahead: a photograph of 24 MP is about 120 MB with its copies.
    behind: Option<Behind>,
    /// The last frame drawn in full of the picture on the screen (see `keep_frame`); it goes into `behind` with the picture.
    on_screen: Option<Ready>,
    /// The picture asked for and being read: the window stays alive and the keys are heard meanwhile; the main loop
    /// shows it when `poll_pending` finds it read.
    pending: Option<Preload>,
    /// Whether the last move was towards the end of the folder: that is the neighbour worth reading ahead.
    forward: bool,
    /// The palette for the frames shown while the picture is moved (see `steady_frame`).
    steady: Option<SteadyPalette>,
    /// The quarter turns clockwise (1 to 3) that each page of each file was given with `r` and `R`, kept while the viewer runs: a page
    /// that is turned stays turned when it is left and come back to.
    turns: std::collections::HashMap<(PathBuf, usize), i32>,
    /// The quarter turns that each file was given as a whole with `a` and `A` (added to those of its pages).
    file_turns: std::collections::HashMap<PathBuf, i32>,
    /// Counts the turns, so that a turn is seen as a change that needs the screen drawn again.
    turned: u32,
    /// The zoom and place the reader chose by hand (a zoom key, the wheel, a move): the next page of the same file is shown the
    /// same way, until `0` brings the whole page back or another file is opened. `None`: each page is shown whole.
    kept: Option<Relative>,
    /// What is remembered of the books between runs (nothing, with `--no-state`).
    state: State,
    /// What was chosen for each file seen with `d`, `e` and `o` (the current file's spread is `spread`), and the files whose
    /// remembered turns and choices have been taken from `state` already.
    own: std::collections::HashMap<PathBuf, BookChoice>,
    /// The settings: the defaults, and whether the choices are each book's own or one for all.
    settings: Settings,
    adopted: std::collections::HashSet<PathBuf>,
    /// The word being typed after `/`, or the number after `p` (None: nothing is being asked for).
    prompt: Option<String>,
    /// Whether the prompt asks for a page's number (`p`), not a word to find (`/`).
    asking_page: bool,
    /// The last search of the book on the screen.
    find: Option<Find>,
    /// The contents of the book on the screen, while the list of them is shown (it takes the keys until Enter or Esc).
    toc: Option<Toc>,
    /// The screen of the settings, while it is shown (it takes the keys until Esc).
    menu: Option<Menu>,
    /// The line of the contents that was gone to last (the file, the line): the list opens on it again while its page is on the screen,
    /// whatever the spread (a page of the pair, or three pages) makes of the page.
    toc_choice: Option<(usize, usize)>,
    /// The message of the last key was written on the line at the bottom already: the key needs no drawing of the picture.
    quiet_message: bool,
    /// With PEEKEL_TRACE=file: where the times of the keys and of the drawing are written (see `trace_line`), when the viewer started, and the
    /// drawing that the terminal has not said it has done yet (when it began and what it was).
    trace: Option<PathBuf>,
    trace_start: Instant,
    /// When a page or a file was asked for last (the trace says how long it took to come).
    asked: Instant,
    trace_wait: Option<(Instant, String)>,
    /// The chosen row of a list (the settings if true, else the contents) moved, and the strips are not yet drawn: where it was before the
    /// burst of keys that moved it, and the first line on the screen then. A held arrow is drawn once, at the end (`flush_list_move`).
    list_move: Option<(bool, usize, usize)>,
    /// The Sixel of rows of a list that were drawn (a row is whole cells high, so it can be put at any line), for the window, the cell
    /// and the background they were made for: scrolling a list is writing rows already made.
    row_cache: RowCache,
    /// The text of the book on the screen, made ready for a search (read when the first search is made).
    texts: Option<(PathBuf, find::Index)>,
}

/// A palette made for the frames of a picture that is being moved, and where it was made.
struct SteadyPalette {
    /// The file (its place in the folder) and the page.
    key: (usize, usize),
    scale: f64,
    /// The picture's point at the middle of the window.
    centre: (f64, f64),
    /// None: the picture has few colours, and its frames are exact.
    palette: Option<Steady>,
}

/// A picture of the folder (a page of it, or a spread of pages) being read on a thread of its own.
struct Preload {
    index: usize,
    /// The page the spread begins with.
    page: usize,
    /// How it is put together and how the pages were turned when it was asked for: a read that no longer fits is not used.
    spread: Spread,
    turned: u32,
    /// The picture left last, taken back: no file is read, and the main loop waits for it at once rather than at its next look.
    quick: bool,
    handle: std::thread::JoinHandle<Result<(Loaded, Option<Ready>), DecodeError>>,
}

/// The picture that was on the screen before the one that is (a page of it, or a spread of pages), kept for a step back.
struct Behind {
    index: usize,
    page: usize,
    spread: Spread,
    turned: u32,
    pages: usize,
    shown: usize,
    image: Image,
    /// Its last frame drawn in full, used again if the window and the view are still those.
    frame: Option<Ready>,
}

/// A page of a file read and ready to be looked at (or several, side by side), and how many pages the file has (one, unless it is a
/// PDF or a DjVu file).
struct Loaded {
    image: Image,
    /// The pages of the file that `image` has: more than one for a spread.
    shown: usize,
    pages: usize,
    /// The file is no picture (or cannot be read as one): its card is shown, made for the size of the window.
    card: bool,
    /// Why a file named like a picture is shown as a card.
    note: Option<String>,
}

/// Page `page` of the file read and ready to be looked at. With `cards`, a file that is no picture, or one that cannot be read
/// as one (a cut PNG, an encrypted PDF), gets a card, as it does in Far; a file that cannot be opened at all is an error. Without
/// them a file that is no picture is `NotAPicture`: skipped, not reported.
fn load(path: &Path, page: usize, cards: bool) -> Result<Loaded, DecodeError> {
    match decode_page(path, page) {
        Ok((image, pages)) => Ok(Loaded {
            image: Image::new(image),
            shown: 1,
            pages,
            card: false,
            note: None,
        }),
        Err(e) if (cards || files::card_only(path)) && std::fs::metadata(path).is_ok() => Ok(Loaded {
            image: Image::blank(),
            shown: 1,
            pages: 1,
            card: true,
            note: (files::is_picture(path) && !files::card_only(path)).then(|| e.to_string()),
        }),
        Err(_) if !files::is_picture(path) && std::fs::metadata(path).is_ok() => Err(DecodeError::NotAPicture),
        Err(e) => Err(e),
    }
}

/// How many pixels the pages of a scan (DjVu) are made with.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Detail {
    /// At the scan's own size.
    Whole,
    /// Enough for the spread to fill this room, times the number given (`scans = window`: 1; `finer`: 2, shrunk then as a whole page
    /// is): a page of 20 million pixels shown in a window of 2 needs a tenth of them; a zoom past that makes the page again with more
    /// (`Viewer::enough_pixels`).
    Room((usize, usize), f64),
    /// At least this part of the scan's size (0 to 1).
    Scale(f64),
}

/// What the setting `scans` does, in a few words (the screen of the settings, the message of `v`).
fn scans_words(scans: Scans) -> &'static str {
    match scans {
        Scans::Window => "for the window",
        Scans::Finer => "twice as fine as the window, then shrunk",
        Scans::Whole => "whole, at the size of the scan",
    }
}

/// The pixels the pages of a scan are read with for the setting `scans` and a window of `room`.
fn detail_of(scans: Scans, room: (usize, usize)) -> Detail {
    match scans {
        Scans::Window => Detail::Room(room, 1.0),
        Scans::Finer => Detail::Room(room, 2.0),
        Scans::Whole => Detail::Whole,
    }
}

/// The spread of a file that begins with page `start`, each page turned as `quarters` say (one number for each page of the spread), put
/// together as `spread` says. A file of one page, a card, and a spread that would be too large come out as the first page alone. The
/// pages of a scan are made with the pixels `detail` says, and looked at as the whole pages ([`Image::reduced`]).
#[allow(clippy::too_many_arguments)]
fn load_spread(
    path: &Path,
    start: usize,
    spread: Spread,
    quarters: &[i32],
    cards: bool,
    mark: Option<&str>,
    night: bool,
    detail: Detail,
) -> Result<Loaded, DecodeError> {
    let quarter = |i: usize| quarters.get(i).copied().unwrap_or(0);
    // an SVG is parsed once and kept with its picture, so that a zoom draws the part shown again from its shapes (a turned one is
    // enlarged as a picture is); one that cannot be read gets its card or its error from `load`
    if peekel_core::svg::is_svg(path) {
        if let Ok(drawing) = peekel_core::svg::Drawing::open(path)
            && let Ok(picture) = drawing.render()
        {
            // what the drawing holds that is not drawn is said at the bottom, so that an empty frame is not taken for a fault
            let note = drawing.skipped().note();
            let image = match quarter(0).rem_euclid(4) {
                0 => Image::of_drawing(picture, std::sync::Arc::new(drawing)),
                q => Image::new(turned_rgba(picture, q)),
            };
            return Ok(Loaded {
                image,
                shown: 1,
                pages: 1,
                card: false,
                note,
            });
        }
        return load(path, start, cards);
    }
    let dark = night && is_book(path);
    // the words of the search stand marked on the page (a page with none, or a file with no zones, is left as it is), and a book in the
    // night look is made dark page by page, so that the gap between the pages stays the terminal's own
    let marked = |index: usize, mut image| {
        if let Some(needle) = mark {
            let _ = mark_words(path, index, needle, &mut image);
        }
        if dark {
            // a page that is no paper (a photograph, a cover) is left as it is
            let _ = peekel_core::night::darken(&mut image);
        }
        image
    };
    // the pages of the spread are read at the same time (the first says how many pages the file has, so one past the end may be read for
    // nothing: a page past the end is the last); the cover that stands alone and a spread of one page need no other
    let wanted = if spread.count <= 1 || (spread.cover_alone && start == 0) {
        1
    } else {
        spread.count
    };
    if detail != Detail::Whole
        && peekel_core::djvu::is_djvu(path)
        && let Some(loaded) = load_scan_smaller(path, start, wanted, spread, &quarter, &marked, detail)
    {
        return Ok(loaded);
    }
    let mut read: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..wanted)
            .map(|i| scope.spawn(move || decode_page(path, start + i)))
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err(DecodeError::Unreadable("reading the page failed".to_string())))
            })
            .collect()
    });
    let Some(Ok((first, pages))) = (!read.is_empty()).then(|| read.remove(0)) else {
        // a card, or the reason why there is none
        return load(path, start, cards);
    };
    let len = spread.len_at(start, pages);
    let mut shown = vec![turned_rgba(marked(start, first), quarter(0))];
    for (i, next) in read.into_iter().enumerate().take(len.saturating_sub(1)) {
        match next {
            Ok((next, _)) => shown.push(turned_rgba(marked(start + i + 1, next), quarter(i + 1))),
            Err(_) => break,
        }
    }
    let count = shown.len();
    let (image, shown) = match (count > 1).then(|| side_by_side(&shown, spread.equal_height)).flatten() {
        Some(together) => (together, count),
        None => (shown.swap_remove(0), 1),
    };
    Ok(Loaded {
        image: Image::new(image),
        shown,
        pages,
        card: false,
        note: None,
    })
}

/// The spread of a DjVu file made smaller than its scans, as `detail` says: the pages read at the same time, each at the same part of
/// its size, so that they keep their sizes against each other, then marked, darkened and turned as the whole pages would be and put
/// together. `None` when the whole pages are wanted after all (the room is not much smaller than the spread), or the first page
/// cannot be read this way: the caller reads them whole, as before.
fn load_scan_smaller(
    path: &Path,
    start: usize,
    wanted: usize,
    spread: Spread,
    quarter: &dyn Fn(usize) -> i32,
    marked: &dyn Fn(usize, peekel_core::view::RgbaImage) -> peekel_core::view::RgbaImage,
    detail: Detail,
) -> Option<Loaded> {
    use peekel_core::view::{fit_scale, side_by_side_size};
    let (first_size, pages) = peekel_core::djvu::page_size(path, start).ok()?;
    let count = wanted.min(spread.len_at(start, pages)).max(1);
    // the size of each page as the scan has it, and as it is shown (turned)
    let mut own = vec![first_size];
    for i in 1..count {
        match peekel_core::djvu::page_size(path, start + i) {
            Ok((size, _)) => own.push(size),
            Err(_) => break,
        }
    }
    let shown_size = |i: usize, (w, h): (u32, u32)| if quarter(i).rem_euclid(2) == 1 { (h, w) } else { (w, h) };
    let shown: Vec<_> = own.iter().enumerate().map(|(i, &s)| shown_size(i, s)).collect();
    // a spread too large for one picture is its first page alone, as with the whole pages
    let shown = match side_by_side_size(&shown, spread.equal_height) {
        Some(_) => shown,
        None => shown[..1].to_vec(),
    };
    let whole = side_by_side_size(&shown, spread.equal_height)?;
    let scale = match detail {
        Detail::Room(room, more) => (fit_scale(whole, room) * more).min(1.0),
        Detail::Scale(scale) => scale,
        Detail::Whole => 1.0,
    };
    // above a half a page is made small directly when it can be (a black and white scan), else it comes whole (see below)
    if scale >= 0.999 {
        return None;
    }
    let read: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..shown.len())
            .map(|i| scope.spawn(move || peekel_core::djvu::render_page_scaled(path, start + i, scale)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().ok().and_then(Result::ok))
            .collect()
    });
    // every page came whole (above a half, a page that could not be made small directly): the spread is the whole pages, as when
    // they are read whole, not shrunk here for nothing
    let all_whole = scale > 0.5
        && read.iter().enumerate().all(|(i, page)| {
            page.as_ref()
                .is_some_and(|(picture, _)| own.get(i) == Some(&picture.dimensions()))
        });
    let mut pictures = Vec::new();
    for (i, page) in read.into_iter().enumerate() {
        let Some((mut picture, _)) = page else { break };
        if all_whole {
            pictures.push(turned_rgba(marked(start + i, picture), quarter(i)));
            continue;
        }
        // each page at exactly its part of its size, so that the pages keep their sizes against each other and the marks fall right
        let (w, h) = own[i];
        let side = |n: u32| ((f64::from(n) * scale).round() as u32).max(1);
        if picture.dimensions() != (side(w), side(h)) {
            picture = peekel_core::resize::triangle(&picture, side(w), side(h));
        }
        pictures.push(turned_rgba(marked(start + i, picture), quarter(i)));
    }
    if pictures.is_empty() {
        return None;
    }
    let shown_count = pictures.len();
    let whole = side_by_side_size(&shown[..shown_count], spread.equal_height)?;
    let picture = if shown_count > 1 {
        side_by_side(&pictures, spread.equal_height)?
    } else {
        pictures.swap_remove(0)
    };
    Some(Loaded {
        image: if all_whole {
            Image::new(picture)
        } else {
            Image::reduced(picture, whole)
        },
        shown: shown_count,
        pages,
        card: false,
        note: None,
    })
}

/// A file the night look is for: a book or a page to read (PDF, DjVu, FB2, EPUB, HTML, Word), not a picture or a card.
fn is_book(path: &Path) -> bool {
    peekel_core::pdf::is_pdf(path)
        || peekel_core::djvu::is_djvu(path)
        || peekel_core::fb2::is_fb2(path)
        || peekel_core::epub::is_epub(path)
        || peekel_core::html::is_html(path)
        || peekel_core::docx::is_docx(path)
}

fn write_out(parts: &[&str]) -> io::Result<()> {
    let mut out = io::stdout().lock();
    for part in parts {
        out.write_all(part.as_bytes())?;
    }
    out.flush()
}

fn write_failed(e: &io::Error) -> String {
    format!("cannot write to the terminal: {e}")
}

impl Viewer {
    fn room(&self) -> (usize, usize) {
        room(self.window, self.cell)
    }

    /// The pixels the pages of a scan are read with, as the settings say.
    fn detail(&self) -> Detail {
        detail_of(self.settings.scans, self.room())
    }

    /// When the picture is a smaller copy of the pages (a scan read for the window) and the view shows it enlarged (a zoom, a window
    /// made larger), the spread is read again with the pixels the view needs, and a little more so that the next step of a zoom needs no
    /// reading; the whole pages when that is most of them. The zoom and the place stay. A spread that cannot be read again stays as it is.
    fn enough_pixels(&mut self) {
        if self.card {
            return;
        }
        let room = self.room();
        let Some(scale) = self.image.short(&self.view, room) else {
            return;
        };
        let Some(path) = self.files.get(self.index).cloned() else {
            return;
        };
        let (index, page, spread) = (self.index, self.page, self.spread);
        let quarters: Vec<i32> = (0..spread.count).map(|i| self.quarters_for(index, page + i)).collect();
        let mark = self.find.as_ref().map(|f| peekel_core::djvu::normalise(&f.needle));
        let detail = Detail::Scale((scale * 1.5).min(1.0));
        let Ok(loaded) = load_spread(
            &path,
            page,
            spread,
            &quarters,
            false,
            mark.as_deref(),
            self.settings.night,
            detail,
        ) else {
            return;
        };
        if loaded.card || loaded.shown != self.shown {
            return;
        }
        let (before, after) = (self.image.size(), loaded.image.size());
        if before != after {
            // (the same pages give the same size; kept in step all the same)
            self.view = self
                .view
                .relative(before, room)
                .map_or_else(|| View::new(after), |r| View::from_relative(r, after, room));
        }
        self.image = loaded.image;
    }

    /// The window's pixels for the current zoom and position, as Sixel.
    /// With `coarse` (the strength of the dithering) the palette is the one made once for the whole picture and the
    /// dithering is the cheap ordered kind: a frame for the moment while the picture is being moved, a fraction of the
    /// work and of the bytes, and the colours are the picture's own.
    fn frame(&mut self, coarse: Option<i32>) -> Result<Rendered, String> {
        let room = self.room();
        let showing_info = self.show_info || self.help || self.toc.is_some() || self.menu.is_some();
        let canvas = self.canvas();
        // the frame made with the read, while it is still what would be made now
        if let Some(ready) = self.ready.take()
            && coarse.is_none()
            && !showing_info
            && ready.canvas == canvas
            && ready.view == self.view
        {
            return Ok(ready.rendered);
        }
        // a page of a scan read for the window and now shown larger is read again with more pixels (not for the quick frames of a move)
        if coarse.is_none() && !showing_info {
            self.enough_pixels();
        }
        if showing_info {
            self.make_info(room);
            // the properties are a picture of their own that fits the window; the picture's zoom is not theirs
            if let Some(info) = self.info.as_mut() {
                let fit = View::new(info.size());
                return picture_frame(info, &fit, canvas, false, |argb, width| (quantize(argb, width), false));
            }
        }
        match coarse {
            Some(dither) => {
                // `steady_frame` borrows the viewer, so the picture is lent out for the frame
                let mut image = std::mem::replace(&mut self.image, Image::blank());
                let (view, size) = (self.view, image.size());
                let made = picture_frame(&mut image, &view, canvas, false, |argb, width| {
                    self.steady_frame(size, argb, width, dither)
                });
                self.image = image;
                made
            }
            None => picture_frame(&mut self.image, &self.view, canvas, true, |argb, width| {
                (quantize(argb, width), false)
            }),
        }
    }

    /// Keeps the frame just drawn when it is the picture's own and drawn in full, to go with the picture into `behind` when it is left.
    fn keep_frame(&mut self, rendered: Rendered) {
        let showing_info = self.show_info || self.help || self.toc.is_some() || self.menu.is_some();
        self.on_screen = (!rendered.rough && !showing_info && !self.card).then(|| Ready {
            canvas: self.canvas(),
            view: self.view,
            rendered,
        });
    }

    fn canvas(&self) -> Canvas {
        Canvas {
            window: self.window,
            cell: self.cell,
            told: self.told,
            background: self.background,
        }
    }

    /// A frame of the picture for the moment, as palette indexes, and whether it is rougher than the full one would be.
    /// The palette is made once for the picture (from a small copy of the whole of it) and kept for as long as the same page
    /// of the same file is shown; a picture of few colours has no need of it and is drawn exactly.
    fn steady_frame(&mut self, size: Size, argb: &[u32], width: usize, dither: i32) -> (Indexed, bool) {
        let room = self.room();
        let scale = self.view.scale(size, room);
        let centre = self.view.center;
        let (visible_w, visible_h) = self.view.visible(size, room);
        let key = (self.index, self.page);
        // the palette is made from what is on the screen, and again when the zoom changes or the window has moved far from
        // where it was made (a small patch of its own colours, a shadow, would have too few of them otherwise)
        let fresh = self.steady.as_ref().is_some_and(|s| {
            s.key == key
                && (s.scale - scale).abs() < 1e-9
                && (s.centre.0 - centre.0).abs() <= visible_w * 0.4
                && (s.centre.1 - centre.1).abs() <= visible_h * 0.4
        });
        if !fresh {
            const EVERY: usize = 3; // one pixel in nine is enough for a palette
            let sample: Vec<u32> = argb
                .chunks(width.max(1))
                .step_by(EVERY)
                .flat_map(|row| row.iter().step_by(EVERY).copied())
                .collect();
            self.steady = Some(SteadyPalette {
                key,
                scale,
                centre,
                palette: Steady::new(&sample),
            });
        }
        match self.steady.as_mut().and_then(|s| s.palette.as_mut()) {
            Some(steady) => (steady.quantize(argb, width, dither), true),
            None => (quantize(argb, width), false),
        }
    }

    /// Makes the picture of the properties of the file for a window of `room` pixels, unless there is one of that size.
    fn make_info(&mut self, room: (usize, usize)) {
        if self
            .info
            .as_ref()
            .is_some_and(|i| (i.size().0 as usize, i.size().1 as usize) == room)
        {
            return;
        }
        let Some(path) = self.files.get(self.index).cloned() else {
            return;
        };
        // the lines of the list that fit, the chosen one always among them
        let shape = self.list_shape_in(room);
        self.toc_fit(shape.rows);
        self.menu_fit(shape.rows);
        self.info = if let Some(list) = self.toc_list(shape.rows) {
            peekel_core::card::render_list(
                "Contents",
                &list.subtitle,
                &list.lines,
                list.marked,
                list.first,
                list.total,
                room.0,
                room.1,
                self.cell_pair(),
                self.background,
            )
            .ok()
        } else if self.menu.is_some() {
            let list = self.menu_list(shape.rows);
            peekel_core::card::render_list(
                "Settings",
                &list.subtitle,
                &list.lines,
                list.marked,
                list.first,
                list.total,
                room.0,
                room.1,
                self.cell_pair(),
                self.background,
            )
            .ok()
        } else if self.help {
            let rows: Vec<(String, String)> = HELP_ROWS
                .iter()
                .map(|(k, m)| ((*k).to_string(), (*m).to_string()))
                .collect();
            peekel_core::card::render_rows("Keys", "any key goes back", &rows, room.0, room.1, self.background).ok()
        } else if self.card {
            // the card of a file that is no picture: the plugin's own, with the sample of a font and the icon of a program
            peekel_core::card::render(&path, room.0, room.1, self.background).ok()
        } else {
            let info = peekel_core::info::of(&path);
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            peekel_core::card::render_rows(&info.kind, &name, &info.rows, room.0, room.1, self.background).ok()
        }
        .map(Image::new);
    }

    /// The line at the bottom: where we are, and the hint (or the message in its place), cut to the window's width.
    fn status_line(&self, scale: f64) -> String {
        let name = self
            .files
            .get(self.index)
            .and_then(|p| p.file_name())
            .map_or_else(|| "?".to_string(), |n| plain(&n.to_string_lossy()));
        let (w, h) = self.image.size();
        let page = if self.pages > 1 && self.shown > 1 {
            format!(" p.{}-{}/{}", self.page + 1, self.page + self.shown, self.pages)
        } else if self.pages > 1 {
            format!(" p.{}/{}", self.page + 1, self.pages)
        } else {
            String::new()
        };
        let found = self.find.as_ref().map(|f| {
            let at = f.at.map_or_else(|| "-".to_string(), |i| (i + 1).to_string());
            format!("/{} {at}/{} pages (n, N)", f.needle, f.hits.len())
        });
        let tail = format!(
            "{}/{} | {}",
            self.index + 1,
            self.files.len(),
            self.message.as_deref().or(found.as_deref()).unwrap_or(HINT),
        );
        // for a DjVu book, how its pages are drawn (`v` changes it)
        let scans = if self
            .files
            .get(self.index)
            .is_some_and(|p| peekel_core::djvu::is_djvu(p))
        {
            format!(" (v: {})", self.settings.scans.word())
        } else {
            String::new()
        };
        let text = if self.card {
            format!(" {name} | card | {tail}")
        } else {
            format!(
                " {name}{page} | {w}x{h}{scans} | {}% | {tail}",
                (scale * 100.0).round() as u32
            )
        };
        let width = usize::from(self.window.cols).saturating_sub(1);
        let mut line: String = text.chars().take(width).collect();
        let pad = width.saturating_sub(line.chars().count());
        line.extend(std::iter::repeat_n(' ', pad));
        line
    }

    /// Draws the picture (and the line at the bottom); with `ask_status` follows it with the status question. When
    /// writing began and how long it took to hand everything to the terminal. The screen is cleared first when
    /// `clear` is set or when the picture is not where the last one was.
    fn paint(&mut self, rendered: &Rendered, clear: bool, ask_status: bool) -> Result<(Instant, Duration), String> {
        let place = Drawn {
            size: rendered.size,
            origin: origin(self.window, self.cell, rendered.size),
        };
        // a picture of another size or place: the room round it is cleared and the last picture stays under it until the new one
        // covers it (`layout::clear_round`); the whole screen only when asked or when what is on it is not known
        let wipe = if clear || self.drawn.is_none() {
            "\x1b[2J".to_string()
        } else if self.drawn.as_ref() != Some(&place) {
            layout::clear_round(self.window, self.cell, place.origin, place.size)
        } else {
            String::new()
        };
        let at = format!("\x1b[{};{}H", place.origin.0, place.origin.1);
        let status = if self.options.status && self.window.rows > 1 {
            format!(
                "\x1b[{};1H\x1b[7m{}\x1b[0m",
                self.window.rows,
                self.status_line(rendered.scale)
            )
        } else {
            String::new()
        };
        let tracing = self.trace.is_some() && !ask_status;
        let question = if ask_status || tracing { STATUS_QUESTION } else { "" };
        let started = Instant::now();
        // the frame is held back by a terminal that knows synchronised output until it is whole (the wipe and the picture
        // are not seen one after the other); one that does not know it ignores the two lines
        write_out(&[BEGIN_FRAME, &wipe, &at, &rendered.stream, &status, END_FRAME, question])
            .map_err(|e| write_failed(&e))?;
        self.drawn = Some(place);
        if tracing {
            let label = format!(
                "picture {}x{}, {} bytes of Sixel; made in {} ms; written in {} ms",
                rendered.size.0,
                rendered.size.1,
                rendered.stream.len(),
                (rendered.scaling + rendered.quantizing + rendered.encoding).as_millis(),
                started.elapsed().as_millis()
            );
            self.trace_wait = Some((started, label));
        }
        Ok((started, started.elapsed()))
    }

    /// With `PEEKEL_TRACE=file`: one line at the end of the file, with the seconds since the viewer started. For finding out where a
    /// delay is (the terminal's own part is told by its answer to "are you done?" asked after each drawing). Nothing otherwise.
    fn trace_line(&self, text: &str) {
        let Some(path) = &self.trace else {
            return;
        };
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{:8.3} s  {text}", self.trace_start.elapsed().as_secs_f64());
        }
    }
    /// Writes only the line at the bottom (while a picture is being read, which takes a moment).
    fn say(&self, text: &str) -> Result<(), String> {
        if !self.options.status || self.window.rows < 2 {
            return Ok(());
        }
        let width = usize::from(self.window.cols).saturating_sub(1);
        let mut line: String = format!(" {text}").chars().take(width).collect();
        line.extend(std::iter::repeat_n(' ', width.saturating_sub(line.chars().count())));
        write_out(&[&format!("\x1b[{};1H\x1b[7m{line}\x1b[0m", self.window.rows)]).map_err(|e| write_failed(&e))
    }

    /// [`Self::go_to`], remembering which way it went, then reads the next picture in that way ahead of the key.
    fn step(&mut self, index: usize, forward: bool) -> Result<(), String> {
        self.forward = forward;
        self.go_to(index)
    }

    /// Starts reading what is next in the direction of the last move unless that is already under way: the next spread of a file of
    /// many pages, else the neighbouring file (from the page it was left on). One at a time: a 20 MP photo is 80 MB.
    fn read_ahead(&mut self) {
        // `ahead` counts from the last page shown when going on and from the first one when going back
        let from = if self.forward {
            self.page + self.shown - 1
        } else {
            self.page
        };
        let Some((next, page)) = ahead(self.index, self.files.len(), from, self.pages, self.forward) else {
            return;
        };
        let spread = self.spread_for(next);
        let page = if next == self.index {
            spread.start_of(page)
        } else {
            self.first_page(next, false)
        };
        if self
            .preload
            .as_ref()
            .is_some_and(|p| self.preload_fits(p, next, page, spread))
            || self
                .behind
                .as_ref()
                .is_some_and(|b| self.behind_fits(b, next, page, spread))
        {
            return;
        }
        self.preload = self.start_read(next, page, spread, true);
    }

    /// Whether a read that was started is the one wanted now: the same pages, put together and turned the same way.
    fn preload_fits(&self, read: &Preload, index: usize, page: usize, spread: Spread) -> bool {
        read.index == index && read.page == page && read.spread == spread && read.turned == self.turned
    }

    /// Starts a thread that reads the spread of file `index` that begins with page `page`; `ahead`: before the key, so its frame is
    /// made there too. A read asked for by a key makes none: the frame is made as soon as it is taken in, and the keys that wait for
    /// a read to end (Home, End, Page Down in a book) are not held longer.
    fn start_read(&self, index: usize, page: usize, spread: Spread, ahead: bool) -> Option<Preload> {
        let path = self.files.get(index).cloned()?;
        let cards = self.cards();
        let quarters: Vec<i32> = (0..spread.count).map(|i| self.quarters_for(index, page + i)).collect();
        let mark = self.find.as_ref().map(|f| peekel_core::djvu::normalise(&f.needle));
        let night = self.settings.night;
        let detail = self.detail();
        let canvas = ahead.then(|| self.canvas());
        Some(Preload {
            index,
            page,
            spread,
            turned: self.turned,
            quick: false,
            handle: std::thread::spawn(move || {
                let mut loaded = load_spread(&path, page, spread, &quarters, cards, mark.as_deref(), night, detail)?;
                // the frame too, whole and in the middle of the window, as a picture is first shown: the key waits only for the writing
                let ready = canvas.filter(|_| !loaded.card).and_then(|canvas| {
                    let view = View::new(loaded.image.size());
                    picture_frame(&mut loaded.image, &view, canvas, true, |argb, width| {
                        (quantize(argb, width), false)
                    })
                    .ok()
                    .map(|rendered| Ready { canvas, view, rendered })
                });
                Ok((loaded, ready))
            }),
        })
    }

    /// Whether the picture left last is the one wanted now: the same pages, put together and turned the same way, and no search marks.
    fn behind_fits(&self, behind: &Behind, index: usize, page: usize, spread: Spread) -> bool {
        behind.index == index
            && behind.page == page
            && behind.spread == spread
            && behind.turned == self.turned
            && self.find.is_none()
    }

    /// The read that was started ahead of the key when it is the one wanted, or the picture left last when it is that one (its
    /// frame is made on a thread, as a read ahead's is), else a new read.
    fn take_read(&mut self, index: usize, page: usize) -> Option<Preload> {
        let spread = self.spread_for(index);
        if let Some(behind) = self.behind.take() {
            if self.behind_fits(&behind, index, page, spread) {
                let canvas = self.canvas();
                let turned = self.turned;
                return Some(Preload {
                    index,
                    page,
                    spread,
                    turned,
                    quick: true,
                    handle: std::thread::spawn(move || {
                        let Behind {
                            pages,
                            shown,
                            mut image,
                            frame,
                            ..
                        } = behind;
                        // the frame it was drawn with, while the window is the same; `frame` looks at the view
                        let ready = frame.filter(|f| f.canvas == canvas).or_else(|| {
                            let view = View::new(image.size());
                            picture_frame(&mut image, &view, canvas, true, |argb, width| {
                                (quantize(argb, width), false)
                            })
                            .ok()
                            .map(|rendered| Ready { canvas, view, rendered })
                        });
                        let loaded = Loaded {
                            image,
                            shown,
                            pages,
                            card: false,
                            note: None,
                        };
                        Ok((loaded, ready))
                    }),
                });
            }
            self.behind = Some(behind);
        }
        match self.preload.take() {
            Some(read) if self.preload_fits(&read, index, page, spread) => Some(read),
            _ => self.start_read(index, page, spread, false),
        }
    }

    /// The remembered book of file `index`, if there is one and the file has not changed.
    fn book_of(&self, index: usize) -> Option<&Book> {
        let path = self.files.get(index)?;
        let size = std::fs::metadata(path).ok()?.len();
        self.state.get(&state::key_of(path), size)
    }

    /// Takes what is remembered of file `index` (its turns, whether its first page stands alone) once, the first time it is wanted.
    fn adopt(&mut self, index: usize) {
        let Some(path) = self.files.get(index).cloned() else {
            return;
        };
        if !self.adopted.insert(path.clone()) {
            return;
        }
        if let Some(book) = self.book_of(index).cloned() {
            self.own.insert(
                path.clone(),
                BookChoice {
                    count: book.count,
                    equal_height: book.equal_height,
                    cover_alone: book.cover_alone,
                },
            );
            if book.file_turn != 0 {
                self.file_turns.insert(path.clone(), book.file_turn);
            }
            for (page, quarters) in book.turns {
                self.turns.insert((path.clone(), page), quarters);
            }
        }
    }

    /// How the pages of file `index` are put together: what was chosen for that book where there is a choice, the defaults elsewhere (see
    /// `Settings::spread`).
    fn spread_for(&mut self, index: usize) -> Spread {
        if index == self.index {
            return self.spread;
        }
        self.adopt(index);
        let own = self
            .files
            .get(index)
            .and_then(|p| self.own.get(p))
            .copied()
            .unwrap_or_default();
        self.settings.spread(own)
    }
    /// The page file `index` is opened on: where it was left, if that is remembered. A book whose text flows that was left at another
    /// size of the text is laid out to find its place (`exact`, the book that is opened now); a book read ahead keeps the page it had.
    fn first_page(&mut self, index: usize, exact: bool) -> usize {
        let spread = self.spread_for(index);
        let path = self.files.get(index).cloned();
        self.book_of(index).map_or(0, |b| match &path {
            Some(path) if exact => spread.start_of(page_of(path, b)),
            _ => spread.start_of(b.page),
        })
    }

    /// Writes down where the book on the screen is (a file of many pages only): the page, how its pages are put together, the zoom
    /// and the turns.
    fn remember(&mut self) {
        if !self.options.remember || self.card || self.pages < 2 {
            return;
        }
        let Some(path) = self.files.get(self.index).cloned() else {
            return;
        };
        let Ok(size) = std::fs::metadata(&path).map(|m| m.len()) else {
            return;
        };
        let mut turns: Vec<(usize, i32)> = self
            .turns
            .iter()
            .filter(|((p, _), _)| *p == path)
            .map(|((_, page), q)| (*page, *q))
            .collect();
        turns.sort_unstable();
        let own = self.own.get(&path).copied().unwrap_or_default();
        let book = Book {
            size,
            page: self.page,
            cover_alone: own.cover_alone,
            count: own.count,
            equal_height: own.equal_height,
            kept: self.kept,
            file_turn: self.file_turns.get(&path).copied().unwrap_or(0),
            turns,
            place: text_flows(&path)
                .then(|| place_of(&path, self.page).ok().flatten())
                .flatten()
                .map(|place| (place, peekel_core::book::text_size())),
        };
        self.state.put(&state::key_of(&path), book);
        self.state.save();
    }

    /// The picture the viewer is on or is going to: the one being read, when there is one.
    fn target(&self) -> usize {
        self.pending.as_ref().map_or(self.index, |p| p.index)
    }

    /// Asks for another picture of the folder. It is read on a thread (a scanned PDF takes seconds) and shown by
    /// `poll_pending` when it is ready; meanwhile keys are heard, and another request replaces this one.
    fn go_to(&mut self, index: usize) -> Result<(), String> {
        let Some(path) = self.files.get(index).cloned() else {
            return Ok(());
        };
        if index == self.target() {
            return Ok(());
        }
        if index == self.index {
            // back to the picture that is shown: the request that was under way is dropped (its thread ends alone)
            self.pending = None;
            return Ok(());
        }
        self.say(&format!(
            "reading {}…",
            plain(&path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default())
        ))?;
        self.remember();
        let page = self.first_page(index, true);
        self.pending = self.take_read(index, page);
        Ok(())
    }

    /// Asks for the spread of the file that is shown that begins with page `page` (it is read on a thread like a file is, or taken
    /// from the one that was started ahead of the key when it is that one). Another request that was under way is dropped.
    fn go_page(&mut self, page: usize) -> Result<(), String> {
        if page >= self.pages {
            return Ok(());
        }
        self.forward = page > self.page;
        self.say(&format!("reading page {}…", page + 1))?;
        self.asked = Instant::now();
        self.pending = self.take_read(self.index, page);
        Ok(())
    }

    /// Takes the requested picture in when its thread has finished; whether the screen has to be drawn again.
    fn poll_pending(&mut self) -> bool {
        if !self.pending.as_ref().is_some_and(|p| p.quick || p.handle.is_finished()) {
            return false;
        }
        let Some(Preload {
            index, page, handle, ..
        }) = self.pending.take()
        else {
            return false;
        };
        self.trace_line(&format!(
            "page {} (file {}) was ready {} ms after it was asked for",
            page + 1,
            index + 1,
            self.asked.elapsed().as_millis()
        ));
        let loaded = handle
            .join()
            .unwrap_or_else(|_| Err(DecodeError::Unreadable("reading the file failed".to_string())));
        match loaded {
            Ok((
                Loaded {
                    image,
                    shown,
                    pages,
                    card,
                    note,
                },
                ready,
            )) => {
                let (left_spread, marked) = (self.spread, self.find.is_some());
                // (the pages were turned as they were turned before when they were read)
                // another file is put together as it was last time (its own cover) and shown as it was left, if that is remembered;
                // another page of the same book keeps the zoom and the place the reader chose
                if index != self.index {
                    self.find = None;
                    self.texts = None;
                    self.spread = self.spread_for(index);
                    self.kept = self.book_of(index).and_then(|b| b.kept);
                }
                if card {
                    self.kept = None;
                }
                let page = self.spread.start_of(page.min(pages.saturating_sub(1)));
                self.view = match self.kept {
                    Some(relative) => View::from_relative(relative, image.size(), self.room()),
                    None => View::new(image.size()),
                };
                let left = std::mem::replace(&mut self.image, image);
                // the picture left is kept, so that a step back needs no reading (not a card, which is made for the window, nor a
                // page with the words of a search marked on it)
                self.behind = (!self.card && !marked).then_some(Behind {
                    index: self.index,
                    page: self.page,
                    spread: left_spread,
                    turned: self.turned,
                    pages: self.pages,
                    shown: self.shown,
                    image: left,
                    frame: self.on_screen.take(),
                });
                self.on_screen = None;
                // (a frame for another view, a page shown at the zoom the reader chose, is not used: `frame` looks)
                self.ready = ready;
                self.index = index;
                self.pages = pages;
                self.page = page;
                self.shown = shown;
                self.steady = None;
                self.card = card;
                self.show_info = card;
                self.info = None;
                self.message = note.map(|n| plain(&n));
                // `drawn` is kept: the last picture stays until the next one covers it, and only the room round the next one is
                // cleared (`paint`, `layout::clear_round`)
                self.remember();
                self.read_ahead();
            }
            // the name and the reason: the whole path would push the reason off the end of the line
            Err(e) => {
                let name = self
                    .files
                    .get(index)
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default();
                self.message = Some(plain(&format!("{name}: {e}")));
            }
        }
        true
    }

    /// What the mouse did; whether the picture has to be drawn again. The wheel zooms; the left button drags the
    /// picture; a click without a drag in the left or the right fifth of the window goes to the previous or the next
    /// file.
    fn mouse(&mut self, button: u32, column: u32, row: u32, press: bool) -> Result<bool, String> {
        const WHEEL: u32 = 64;
        const MOVE: u32 = 32;
        const KEYS: u32 = 4 | 8 | 16; // Shift, Alt, Ctrl: not told apart
        let button = button & !KEYS;
        if self.help {
            // a press takes the list of keys away, as a key does
            return Ok(press && button < MOVE && self.close_help());
        }
        // the settings: the wheel moves the chosen row, a click changes it
        if self.menu.is_some() {
            return self.menu_mouse(button, row, press);
        }
        // the list of contents: the wheel moves the chosen line, a click goes to the chapter
        if self.toc.is_some() {
            return self.toc_mouse(button, row, press);
        }
        match (button, press) {
            (WHEEL, _) => self.apply(Action::ZoomIn),
            (b, _) if b == WHEEL + 1 => self.apply(Action::ZoomOut),
            (0, true) => {
                self.drag = Some(Drag {
                    column,
                    row,
                    moved: false,
                });
                Ok(false)
            }
            (MOVE, true) => {
                let Some(drag) = self.drag.as_mut() else {
                    return Ok(false);
                };
                let (dx, dy) = (
                    i64::from(column) - i64::from(drag.column),
                    i64::from(row) - i64::from(drag.row),
                );
                if dx == 0 && dy == 0 {
                    return Ok(false);
                }
                drag.column = column;
                drag.row = row;
                drag.moved = true;
                if self.show_info {
                    return Ok(false); // the properties and the cards do not move
                }
                let (size, room) = (self.image.size(), self.room());
                let before = self.view;
                // the picture follows the pointer, so the view goes the other way
                self.view.pan(
                    -(dx as f64) * f64::from(self.cell.width),
                    -(dy as f64) * f64::from(self.cell.height),
                    size,
                    room,
                );
                self.kept = self.view.relative(size, room);
                Ok(before != self.view)
            }
            (0, false) => {
                let Some(drag) = self.drag.take() else {
                    return Ok(false);
                };
                let cols = u32::from(self.window.cols);
                if drag.moved || row >= u32::from(self.window.rows) {
                    Ok(false) // a drag, or a click on the line at the bottom
                } else if column * 5 <= cols {
                    self.apply(Action::Previous)
                } else if column * 5 > cols * 4 {
                    self.apply(Action::Next)
                } else {
                    Ok(false)
                }
            }
            _ => Ok(false),
        }
    }

    /// Takes the list of keys away; true (the picture has to be drawn again).
    fn close_help(&mut self) -> bool {
        self.help = false;
        self.info = None;
        self.drawn = None;
        true
    }

    /// Does what a key asks; whether the picture has to be drawn again.
    fn apply(&mut self, action: Action) -> Result<bool, String> {
        let size = self.image.size();
        let room = self.room();
        let before = (
            self.view,
            self.index,
            self.message.clone(),
            self.show_info,
            self.help,
            self.turned,
            self.toc.is_some(),
            self.menu.is_some(),
        );
        // the list of keys is taken away by any key, which does nothing else
        if self.help {
            return Ok(self.close_help());
        }
        self.message = None;
        let count = self.files.len();
        // the picture's zoom and position mean nothing while its properties are on the screen
        if self.show_info
            && matches!(
                action,
                Action::ZoomIn
                    | Action::ZoomOut
                    | Action::Fit
                    | Action::ActualSize
                    | Action::FitWidth
                    | Action::Pan(..)
            )
        {
            self.message = Some(if self.card {
                "a card has no zoom".to_string()
            } else {
                "properties: i goes back to the picture".to_string()
            });
            return Ok(false);
        }
        match action {
            Action::Quit => {}
            Action::CopyPath => {
                if let Some(path) = self.files.get(self.index) {
                    let full = std::path::absolute(path).unwrap_or_else(|_| path.clone());
                    let sequence = peekel_term::clipboard::copy_sequence(&full.to_string_lossy());
                    write_out(&[&sequence]).map_err(|e| write_failed(&e))?;
                    // the terminal does not say whether it took it
                    self.message = Some("path sent to the clipboard (if the terminal allows it)".to_string());
                }
            }
            Action::CopyMagnet => {
                let link = self
                    .files
                    .get(self.index)
                    .filter(|p| peekel_core::torrent::is_torrent(p))
                    .and_then(|p| peekel_core::torrent::magnet_of(p));
                self.message = Some(match link {
                    Some(link) => {
                        let sequence = peekel_term::clipboard::copy_sequence(&link);
                        write_out(&[&sequence]).map_err(|e| write_failed(&e))?;
                        "magnet link sent to the clipboard (if the terminal allows it)".to_string()
                    }
                    None => "m copies the magnet link of a .torrent".to_string(),
                });
            }
            Action::Help => {
                self.help = true;
                self.info = None;
                self.drawn = None;
            }
            // a file that is no picture has no picture to go back to
            Action::Info if self.card => {}
            Action::Info => {
                self.show_info = !self.show_info;
                self.drawn = None;
                if self.show_info {
                    self.message = Some("properties: i goes back to the picture".to_string());
                }
            }
            Action::ZoomIn => self.view.zoom_by(ZOOM_STEP, size, room),
            Action::ZoomOut => self.view.zoom_by(1.0 / ZOOM_STEP, size, room),
            Action::Fit => self.view.fit(size),
            Action::ActualSize => self.view.actual_size(size, room),
            Action::FitWidth => self.view.fit_width(size, room),
            Action::Pan(dx, dy) => self.view.pan(
                f64::from(dx) * room.0 as f64 / 8.0,
                f64::from(dy) * room.1 as f64 / 8.0,
                size,
                room,
            ),
            // a document of many pages is turned by Page Down and Page Up; past its last page (or before its first) they
            // go on to the next file (or the previous one), and in a single picture they always do
            Action::PageDown if self.pending.is_none() && self.page + self.shown < self.pages => {
                self.go_page(self.page + self.shown)?
            }
            Action::PageUp if self.pending.is_none() && self.spread.before(self.page).is_some() => {
                self.go_page(self.spread.before(self.page).unwrap_or(0))?
            }
            Action::PageDown | Action::Next => self.step((self.target() + 1) % count, true)?,
            Action::PageUp | Action::Previous => self.step((self.target() + count - 1) % count, false)?,
            Action::First => self.step(0, true)?,
            Action::Last => self.step(count - 1, false)?,
            // in a book of many pages Home and End are its beginning and its end; in any other file they are the first and the last file
            Action::BookStart | Action::BookEnd if self.pages > 1 && !self.card => {
                let page = if action == Action::BookStart { 0 } else { self.pages - 1 };
                let start = self.spread.start_of(page);
                if self.pending.is_none() && start != self.page {
                    self.go_page(start)?;
                }
            }
            Action::BookStart => self.step(0, true)?,
            Action::BookEnd => self.step(count - 1, false)?,
            Action::TurnRight | Action::TurnLeft | Action::TurnAllRight | Action::TurnAllLeft if self.card => {
                self.message = Some("a card has no page to turn".to_string());
            }
            Action::TurnRight | Action::TurnLeft | Action::TurnAllRight | Action::TurnAllLeft if self.show_info => {
                self.message = Some("properties: i goes back to the picture".to_string());
            }
            Action::TurnRight => self.turn(1, false)?,
            Action::TurnLeft => self.turn(-1, false)?,
            Action::TurnAllRight => self.turn(1, true)?,
            Action::TurnAllLeft => self.turn(-1, true)?,
            Action::Search | Action::NextHit | Action::PrevHit if self.card || self.show_info => {
                self.message = Some(if self.card {
                    "a card has no text to look in".to_string()
                } else {
                    "properties: i goes back to the picture".to_string()
                });
            }
            Action::Settings if self.card || self.show_info => {
                self.message = Some(if self.card {
                    "a card has no pages to set up".to_string()
                } else {
                    "properties: i goes back to the picture".to_string()
                });
            }
            Action::Settings => {
                self.menu = Some(Menu {
                    selected: menu_end(true),
                    first: 0,
                });
                self.info = None;
                self.drawn = None;
            }
            Action::Contents if self.card || self.show_info => {
                self.message = Some(if self.card {
                    "a card has no contents".to_string()
                } else {
                    "properties: i goes back to the picture".to_string()
                });
            }
            Action::Contents => self.open_contents()?,
            Action::Search => {
                self.prompt = Some(String::new());
                self.asking_page = false;
                self.message = Some("/".to_string());
            }
            Action::GoToPage if self.show_info && !self.card => {
                self.message = Some("properties: i goes back to the picture".to_string());
            }
            Action::GoToPage if self.card || self.pages < 2 => {
                self.message = Some("this file has one page".to_string());
            }
            Action::GoToPage => {
                self.prompt = Some(String::new());
                self.asking_page = true;
                self.message = Some(format!("page (1-{}): ", self.pages));
            }
            Action::NextHit | Action::PrevHit if self.find.is_none() => {
                self.message = Some("no search yet: / asks for a word".to_string());
            }
            Action::NextHit => self.go_to_hit(true)?,
            Action::PrevHit => self.go_to_hit(false)?,
            Action::Night => self.set_night(!self.settings.night)?,
            Action::Justify if self.show_info && !self.card => {
                self.message = Some("properties: i goes back to the picture".to_string());
            }
            Action::Justify => self.set_book_look(Setting::Justify)?,
            Action::Scans => self.set_scans(self.settings.scans.next()),
            Action::TextLarger | Action::TextSmaller if self.show_info && !self.card => {
                self.message = Some("properties: i goes back to the picture".to_string());
            }
            Action::TextLarger | Action::TextSmaller => {
                let step = peekel_core::book::TEXT_SIZE_STEP;
                let now = self.settings.text_size;
                let wanted = if action == Action::TextLarger {
                    now + step
                } else {
                    now.saturating_sub(step)
                };
                self.set_text_size(wanted)?;
            }
            Action::SpreadPages | Action::SpreadShift | Action::EqualHeight if self.card || self.show_info => {
                self.message = Some(if self.card {
                    "a card has no pages to put side by side".to_string()
                } else {
                    "properties: i goes back to the picture".to_string()
                });
            }
            Action::SpreadPages | Action::SpreadShift | Action::EqualHeight => {
                // what is chosen: for this book (``o``, and ``d`` and ``e`` when each book has its own choice), or for all books (``d`` and
                // ``e`` when the settings say all books follow one choice): that is a change of the settings, and it is written at once
                let mut own = self
                    .files
                    .get(self.index)
                    .and_then(|p| self.own.get(p))
                    .copied()
                    .unwrap_or_default();
                let mut spread = self.spread;
                match action {
                    Action::SpreadPages => {
                        spread = spread.next_count();
                        match self.settings.scope {
                            Scope::EachBook => own.count = Some(spread.count),
                            Scope::AllBooks => {
                                self.settings.pages = spread.count;
                                self.settings.save();
                            }
                        }
                    }
                    Action::SpreadShift => {
                        spread.cover_alone = !spread.cover_alone;
                        own.cover_alone = Some(spread.cover_alone);
                    }
                    _ => {
                        spread.equal_height = !spread.equal_height;
                        match self.settings.scope {
                            Scope::EachBook => own.equal_height = Some(spread.equal_height),
                            Scope::AllBooks => {
                                self.settings.equal_height = spread.equal_height;
                                self.settings.save();
                            }
                        }
                    }
                }
                if let Some(path) = self.files.get(self.index).cloned() {
                    self.own.insert(path, own);
                }
                self.message = Some(match action {
                    Action::SpreadPages => format!("{} page(s) side by side", spread.count),
                    Action::SpreadShift if spread.cover_alone => "the first page alone, then 2-3, 4-5 …".to_string(),
                    Action::SpreadShift => "the pairs are 1-2, 3-4 …".to_string(),
                    _ if spread.equal_height => "pages of a spread made equally high".to_string(),
                    _ => "pages of a spread in their own sizes".to_string(),
                });
                if self.pages > 1 && self.pending.is_none() {
                    self.set_spread(spread)?;
                } else {
                    // a file of one page has nothing to put side by side; the next book is read the new way
                    self.spread = spread;
                    self.preload = None;
                    self.turned += 1;
                }
            }
        }
        if matches!(
            action,
            Action::ZoomIn | Action::ZoomOut | Action::Fit | Action::ActualSize | Action::FitWidth | Action::Pan(..)
        ) {
            self.kept = self.view.relative(size, room);
        }
        // a message that was written on the line at the bottom already needs no drawing of the picture
        let message = if std::mem::take(&mut self.quiet_message) {
            before.2.clone()
        } else {
            self.message.clone()
        };
        Ok(before
            != (
                self.view,
                self.index,
                message,
                self.show_info,
                self.help,
                self.turned,
                self.toc.is_some(),
                self.menu.is_some(),
            ))
    }

    /// The quarter turns clockwise that page `page` of file `index` has: those of the page itself and those of the file as a whole.
    fn quarters_for(&self, index: usize, page: usize) -> i32 {
        self.files.get(index).map_or(0, |path| {
            let own = self.turns.get(&(path.clone(), page)).copied().unwrap_or(0);
            (own + self.file_turns.get(path).copied().unwrap_or(0)).rem_euclid(4)
        })
    }

    /// Turns the pages that are shown a quarter clockwise (`quarters` 1) or counter-clockwise (-1): the page or, in a spread, its pages, or
    /// with `whole_file` every page of the file (those that were turned one by one are turned the same further). It is remembered until
    /// the viewer ends; the picture is shown whole again, as after a change of page.
    fn turn(&mut self, quarters: i32, whole_file: bool) -> Result<(), String> {
        if let Some(path) = self.files.get(self.index).cloned() {
            if whole_file {
                let total = (self.file_turns.get(&path).copied().unwrap_or(0) + quarters).rem_euclid(4);
                if total == 0 {
                    self.file_turns.remove(&path);
                } else {
                    self.file_turns.insert(path, total);
                }
            } else {
                for page in self.page..self.page + self.shown {
                    let key = (path.clone(), page);
                    let total = (self.turns.get(&key).copied().unwrap_or(0) + quarters).rem_euclid(4);
                    if total == 0 {
                        self.turns.remove(&key);
                    } else {
                        self.turns.insert(key, total);
                    }
                }
            }
        }
        self.turned += 1;
        self.kept = None;
        self.remember();
        if self.shown > 1 {
            // the pages are put together again from the turned ones
            return self.go_page(self.page);
        }
        self.image = std::mem::replace(&mut self.image, Image::blank()).turned(quarters);
        self.view = View::new(self.image.size());
        self.steady = None;
        self.drawn = None;
        Ok(())
    }

    /// A key while a word is being typed after `/` (or a number after `p`): a letter is added, Backspace takes one off, Enter looks for
    /// the word (or goes to the page), Esc leaves it. Whether the screen has to be drawn again.
    fn type_key(&mut self, key: &[u8]) -> Result<bool, String> {
        let Some(word) = self.prompt.as_mut() else {
            return Ok(false);
        };
        match key {
            b"\x1b" | b"\x03" => {
                self.prompt = None;
                self.message = None;
            }
            b"\r" | b"\n" => {
                let typed = self.prompt.take().unwrap_or_default();
                self.message = None;
                if self.asking_page {
                    if let Some(page) = page_of_number(&typed, self.pages) {
                        let start = self.spread.start_of(page);
                        if self.pending.is_none() && start != self.page {
                            self.go_page(start)?;
                        }
                    }
                } else {
                    self.search(&typed)?;
                }
            }
            b"\x7f" | b"\x08" => {
                word.pop();
                return self.show_prompt();
            }
            _ => {
                // `key` is a whole character, as the layout made it (see `KeyDecoder`); an escape sequence (an arrow) has control
                // characters in it and is not a letter. A page's number takes digits only.
                if let Ok(letters) = std::str::from_utf8(key)
                    && !letters.chars().any(char::is_control)
                    && (!self.asking_page || letters.chars().all(|c| c.is_ascii_digit()))
                    && word.chars().count() < if self.asking_page { MAX_PAGE_DIGITS } else { MAX_NEEDLE }
                {
                    word.push_str(letters);
                }
                return self.show_prompt();
            }
        }
        Ok(true)
    }

    /// Writes the word that is being typed on the line at the bottom, and only there: the picture is not drawn again for each letter.
    /// (The picture is drawn when the word is entered or given up.)
    fn show_prompt(&mut self) -> Result<bool, String> {
        let typed = self.prompt.as_deref().unwrap_or_default();
        let line = if self.asking_page {
            format!("page (1-{}): {typed}", self.pages)
        } else {
            format!("/{typed}")
        };
        self.message = Some(line.clone());
        self.say(&line)?;
        Ok(false)
    }

    /// Shows the contents of the book (its bookmarks) as a list, the line of the chapter this page is in chosen; or says why there is none.
    fn open_contents(&mut self) -> Result<(), String> {
        let Some(path) = self.files.get(self.index).cloned() else {
            return Ok(());
        };
        self.say("reading the contents…")?;
        match peekel_core::decode::contents(&path) {
            Ok(Some(items)) => {
                // the line that was gone to last, while its page is on the screen; else the chapter of the spread, judged by its last page (a
                // chapter that begins on the right page of the spread is the one to show)
                let last_page = self.page + self.shown.saturating_sub(1);
                let remembered = self
                    .toc_choice
                    .filter(|&(file, _)| file == self.index)
                    .map(|(_, line)| line)
                    .filter(|&line| {
                        items
                            .get(line)
                            .and_then(|item| item.page)
                            .is_some_and(|page| (self.page..=last_page).contains(&page))
                    });
                let selected = remembered.unwrap_or_else(|| chapter_at(&items, last_page));
                self.trace_line(&format!(
                    "contents: line {} of {} chosen for the spread of pages {} to {}",
                    selected + 1,
                    items.len(),
                    self.page + 1,
                    last_page + 1
                ));
                self.toc = Some(Toc {
                    items,
                    selected,
                    first: 0,
                });
                self.info = None;
                self.drawn = None;
            }
            Ok(None) => {
                // said on the line at the bottom at once, and kept as the message: the picture is not drawn again for a word (that takes
                // much longer than the reading did), and when it is drawn later (a move that was drawn roughly is drawn well a moment after) the line has
                // the message still
                let said = if peekel_core::djvu::is_djvu(&path)
                    || peekel_core::pdf::is_pdf(&path)
                    || peekel_core::fb2::is_fb2(&path)
                    || peekel_core::epub::is_epub(&path)
                    || peekel_core::html::is_html(&path)
                    || peekel_core::docx::is_docx(&path)
                {
                    "this book has no contents"
                } else {
                    "only DjVu, PDF, FB2, EPUB, HTML and Word books have a contents"
                };
                self.message = Some(said.to_string());
                self.quiet_message = true;
                self.say(said)?;
            }
            Err(e) => {
                let said = plain(&format!("the contents cannot be read: {e}"));
                self.message = Some(said.clone());
                self.quiet_message = true;
                self.say(&said)?;
            }
        }
        Ok(())
    }

    /// The shape of a list in a window of `room` pixels, with the cell the terminal really has: a row is whole cells high.
    fn list_shape_in(&self, room: (usize, usize)) -> peekel_core::card::ListShape {
        peekel_core::card::list_shape(room.0, room.1, (self.cell.height as usize).max(1))
    }

    /// The cell as a pair of pixels (width, height).
    fn cell_pair(&self) -> (usize, usize) {
        ((self.cell.width as usize).max(1), (self.cell.height as usize).max(1))
    }

    /// How many lines of the list of contents fit in the window.
    fn toc_rows(&self) -> usize {
        let room = self.room();
        self.list_shape_in(room).rows
    }

    /// Closes the list of contents; if `go`, goes to the page of the line that is chosen. Whether the screen has to be drawn at once: not when a page is\n    /// being read (the list stays on the screen with the line at the bottom saying so, and the page is drawn when it is read: drawing the old page\n    /// meanwhile only makes the wait longer).
    fn close_contents(&mut self, go: bool) -> Result<bool, String> {
        let page = self
            .toc
            .as_ref()
            .and_then(|toc| toc.items.get(toc.selected))
            .and_then(|item| item.page);
        if go && page.is_some() {
            self.toc_choice = self.toc.as_ref().map(|toc| (self.index, toc.selected));
        }
        self.toc = None;
        self.info = None;
        self.drawn = None;
        if !go {
            return Ok(true);
        }
        match page {
            Some(page) => {
                let start = self.spread.start_of(page);
                if start != self.page {
                    self.go_page(start)?;
                    return Ok(false);
                }
            }
            None => self.message = Some("this line leads to no page of the book".to_string()),
        }
        Ok(true)
    }

    /// A row of the screen of the settings: what the setting is and what it is now.
    fn setting_row(&self, setting: Setting) -> (&'static str, String) {
        let yes_no = |v: bool| if v { "yes" } else { "no" }.to_string();
        let s = &self.settings;
        match setting {
            Setting::TextSize => ("Size of the text (f, F)", format!("{} %", s.text_size)),
            Setting::Justify => ("Paragraphs justified (t)", yes_no(s.justify)),
            Setting::MarkParagraphs => ("Paragraphs always set apart", yes_no(s.mark_paragraphs)),
            Setting::ChosenSize => ("The text at the size chosen", yes_no(s.chosen_size)),
            Setting::CodeBackdrop => ("Code on a shade", yes_no(s.code_backdrop)),
            Setting::Night => ("Night: the pages dark (b)", yes_no(s.night)),
            Setting::Scope => (
                "Pages side by side and their height are chosen",
                match s.scope {
                    Scope::EachBook => "for each book",
                    Scope::AllBooks => "for all books",
                }
                .to_string(),
            ),
            Setting::Pages => ("Pages side by side (the default)", s.pages.to_string()),
            Setting::EqualHeight => ("Pages made equally high (the default)", yes_no(s.equal_height)),
            Setting::CoverAlone => ("The first page stands alone (the default)", yes_no(s.cover_alone)),
            Setting::Scans => ("Pages drawn (v)", scans_words(s.scans).to_string()),
            Setting::ScansAsDjvulibre => ("Small pages as DjVuLibre makes them", yes_no(s.scans_as_djvulibre)),
            Setting::Browse => (
                "Space and Backspace go through",
                match self.listing() {
                    Listing::Every => "every file (the rest as cards)",
                    Listing::Pictures => "pictures and documents",
                    Listing::Books => "books only",
                }
                .to_string(),
            ),
        }
    }

    /// Which files of the folder are gone through: the setting, or every file with `--cards` (`PEEKEL_CARDS`) for this run.
    fn listing(&self) -> Listing {
        listing(&self.options, &self.settings)
    }

    /// Whether a file that is no picture (or a picture that cannot be read) is shown as a card.
    fn cards(&self) -> bool {
        self.listing() == Listing::Every
    }

    /// Changes the setting of line `line` of `MENU` (to the next value, or with `forward` false the one before), writes the settings,
    /// and puts the pages of the book on the screen together as they now say.
    fn change_setting(&mut self, line: usize, forward: bool) -> Result<(), String> {
        let Some(MenuLine::Row(setting)) = MENU.get(line).copied() else {
            return Ok(());
        };
        match setting {
            Setting::Scope => {
                self.settings.scope = match self.settings.scope {
                    Scope::EachBook => Scope::AllBooks,
                    Scope::AllBooks => Scope::EachBook,
                };
            }
            Setting::Pages => {
                let at = self.settings.pages;
                self.settings.pages = if forward { at % 3 + 1 } else { (at + 1) % 3 + 1 };
            }
            Setting::EqualHeight => self.settings.equal_height = !self.settings.equal_height,
            Setting::CoverAlone => self.settings.cover_alone = !self.settings.cover_alone,
            Setting::Justify | Setting::MarkParagraphs | Setting::ChosenSize | Setting::CodeBackdrop => {
                self.info = None;
                return self.set_book_look(setting);
            }
            Setting::ScansAsDjvulibre => {
                self.set_scans_as_djvulibre(!self.settings.scans_as_djvulibre);
                return Ok(());
            }
            Setting::Browse => {
                let order = [Listing::Pictures, Listing::Books, Listing::Every];
                let at = order.iter().position(|l| *l == self.settings.browse).unwrap_or(0);
                let next = if forward { at + 1 } else { at + order.len() - 1 };
                self.settings.browse = order[next % order.len()];
                // the folder is listed again, the file on the screen kept; what was read ahead or kept was read with the other list
                if let Some(path) = self.files.get(self.index).cloned() {
                    let (files, index) = files::in_folder(&path, self.listing());
                    self.files = files;
                    self.index = index;
                }
                self.preload = None;
                self.behind = None;
                self.toc_choice = None;
                self.turned += 1;
            }
            Setting::TextSize => {
                let step = peekel_core::book::TEXT_SIZE_STEP;
                let now = self.settings.text_size;
                self.info = None;
                return self.set_text_size(if forward { now + step } else { now.saturating_sub(step) });
            }
            Setting::Night => return self.set_night(!self.settings.night),
            Setting::Scans => {
                self.set_scans(if forward {
                    self.settings.scans.next()
                } else {
                    self.settings.scans.next().next()
                });
                return Ok(());
            }
        }
        self.settings.save();
        self.info = None;
        // the book on the screen follows what was chosen for it, or the new defaults
        let own = self
            .files
            .get(self.index)
            .and_then(|p| self.own.get(p))
            .copied()
            .unwrap_or_default();
        let wanted = self.settings.spread(own);
        if wanted != self.spread {
            if self.pages > 1 && self.pending.is_none() && !self.card {
                self.set_spread(wanted)?;
            } else {
                self.spread = wanted;
                self.preload = None;
                self.turned += 1;
            }
        }
        Ok(())
    }

    /// A key while the settings are shown: up and down choose a row; Enter, Space and right change it, left changes it back; Esc, `q` and
    /// `s` close the screen. Whether the screen has to be drawn again.
    fn menu_key(&mut self, key: &[u8]) -> Result<bool, String> {
        let Some(menu) = self.menu.as_mut() else {
            return Ok(false);
        };
        let (before, first_before) = (menu.selected, menu.first);
        match key {
            b"\x1b" | b"\x03" | b"q" | b"Q" | b"s" | b"S" => {
                self.menu = None;
                self.info = None;
                self.drawn = None;
                Ok(true)
            }
            b"\r" | b"\n" | b" " => {
                self.change_setting(before, true)?;
                Ok(true)
            }
            _ => match action(key) {
                Some(Action::Pan(_, dy)) if dy != 0 => {
                    menu.selected = if dy > 0 {
                        menu_next(before, true)
                    } else {
                        menu_next(before, false)
                    };
                    self.menu_moved(before, first_before)
                }
                Some(Action::Pan(dx, _)) if dx != 0 => {
                    self.change_setting(before, dx > 0)?;
                    Ok(true)
                }
                Some(Action::BookStart | Action::First) => {
                    menu.selected = menu_end(true);
                    self.menu_moved(before, first_before)
                }
                Some(Action::BookEnd | Action::Last) => {
                    menu.selected = menu_end(false);
                    self.menu_moved(before, first_before)
                }
                _ => Ok(false),
            },
        }
    }

    /// The mouse while the settings are shown: the wheel moves the chosen row, a click on a row changes it.
    fn menu_mouse(&mut self, button: u32, row: u32, press: bool) -> Result<bool, String> {
        const WHEEL: u32 = 64;
        let room = self.room();
        let shape = self.list_shape_in(room);
        let cell_height = self.cell.height as f32;
        let Some(menu) = self.menu.as_mut() else {
            return Ok(false);
        };
        let (before, first_before) = (menu.selected, menu.first);
        match (button, press) {
            (WHEEL, _) => {
                menu.selected = menu_next(before, false);
                self.menu_moved(before, first_before)
            }
            (65, _) => {
                menu.selected = menu_next(before, true);
                self.menu_moved(before, first_before)
            }
            (0, true) => {
                // the picture is as large as the room, so its pixels are those of the window (rows count from 1)
                let y = row.saturating_sub(1) as f32 * cell_height + cell_height / 2.0;
                let line = peekel_core::card::list_row_at(shape, y).map(|at| menu.first + at);
                match line {
                    // the name of a part is not a setting
                    Some(line) if matches!(MENU.get(line), Some(MenuLine::Row(_))) => {
                        menu.selected = line;
                        self.change_setting(line, true)?;
                        Ok(true)
                    }
                    _ => Ok(false),
                }
            }
            _ => Ok(false),
        }
    }

    /// Keeps the chosen line of the settings among the `rows` on the screen, with the name of its part above it when that is the line
    /// before.
    fn menu_fit(&mut self, rows: usize) {
        if let Some(menu) = self.menu.as_mut() {
            let rows = rows.max(1);
            let top = match menu.selected.checked_sub(1).and_then(|i| MENU.get(i)) {
                Some(MenuLine::Part(_)) => menu.selected - 1,
                _ => menu.selected,
            };
            if top < menu.first {
                menu.first = top;
            } else if menu.selected >= menu.first + rows {
                menu.first = menu.selected + 1 - rows;
            }
            menu.first = menu.first.min(menu_total().saturating_sub(rows));
        }
    }
    /// Keeps the chosen line of the list of contents among the lines that are on the screen (`rows` of them).
    fn toc_fit(&mut self, rows: usize) {
        if let Some(toc) = self.toc.as_mut() {
            if toc.selected < toc.first {
                toc.first = toc.selected;
            } else if toc.selected >= toc.first + rows {
                toc.first = toc.selected + 1 - rows;
            }
            toc.first = toc.first.min(toc.items.len().saturating_sub(rows));
        }
    }

    /// What is drawn of the list of contents: the lines on the screen, the one chosen among them, and the line under the title.
    fn toc_list(&self, rows: usize) -> Option<TocList> {
        let toc = self.toc.as_ref()?;
        let total = toc.items.len();
        let lines: Vec<peekel_core::card::ListLine> = toc
            .items
            .iter()
            .skip(toc.first)
            .take(rows)
            .map(|item| peekel_core::card::ListLine {
                depth: item.depth,
                text: plain(&item.title),
                right: item.page.map_or_else(String::new, |p| format!("p.{}", p + 1)),
                kind: peekel_core::card::LineKind::Item,
            })
            .collect();
        Some(TocList {
            subtitle: format!(
                "{} of {}   click or Enter goes there, Esc closes",
                toc.selected + 1,
                total
            ),
            lines,
            marked: toc.selected.checked_sub(toc.first),
            first: toc.first,
            total,
        })
    }

    /// The chosen line of the list of contents moved (from `before`, the first line on the screen having been `first_before`). When the
    /// same lines are on the screen, only the strips that changed (the heading and the two lines) are drawn, and they are drawn alone, not
    /// cut out of the whole list: a strip costs a fraction of the list, and a terminal draws a few strips much faster than a picture the
    /// size of the window. Whether the whole picture has to be drawn after all (the lines scrolled, or the strips could not be made).
    fn toc_moved(&mut self, before: usize, first_before: usize) -> Result<bool, String> {
        // the whole picture is made when it is wanted (see `make_info`)
        self.info = None;
        let room = self.room();
        let shape = self.list_shape_in(room);
        self.toc_fit(shape.rows);
        if self.toc.is_none() {
            return Ok(true);
        }
        // a terminal whose real cell is not the one it tells needs the picture made over (see `frame`); the strips do not do that
        let on_screen_is_this_size = self.drawn.as_ref().is_some_and(|d| d.size == room);
        if self.cell != self.told || !on_screen_is_this_size {
            return Ok(true);
        }
        let (earliest, earliest_first) = self.list_move.map_or((before, first_before), |(_, b, f)| (b, f));
        self.list_move = Some((false, earliest, earliest_first));
        Ok(false)
    }

    /// Draws the strips of a list whose chosen row moved during the keys just dealt with, once, from where it was before them. Whether
    /// the whole picture has to be drawn after all.
    fn flush_list_move(&mut self) -> Result<bool, String> {
        let Some((menu, before, first_before)) = self.list_move.take() else {
            return Ok(false);
        };
        let room = self.room();
        let shape = self.list_shape_in(room);
        if menu {
            let Some((selected, first)) = self.menu.as_ref().map(|m| (m.selected, m.first)) else {
                return Ok(false);
            };
            if before == selected && first == first_before {
                return Ok(false);
            }
            let list = self.menu_list(shape.rows);
            let scroll = first as isize - first_before as isize;
            self.list_strips("Settings", &list, before, selected, scroll, shape, room)
        } else {
            let Some((selected, first)) = self.toc.as_ref().map(|t| (t.selected, t.first)) else {
                return Ok(false);
            };
            if before == selected && first == first_before {
                return Ok(false);
            }
            let Some(list) = self.toc_list(shape.rows) else {
                return Ok(true);
            };
            let scroll = first as isize - first_before as isize;
            self.list_strips("Contents", &list, before, selected, scroll, shape, room)
        }
    }

    /// The chosen row of the settings moved from `before` (the first line on the screen having been `first_before`): only the strips
    /// that changed are drawn (as for the contents), else the whole picture. Whether the whole picture has to be drawn after all.
    fn menu_moved(&mut self, before: usize, first_before: usize) -> Result<bool, String> {
        self.info = None;
        let room = self.room();
        let shape = self.list_shape_in(room);
        self.menu_fit(shape.rows);
        let Some(selected) = self.menu.as_ref().map(|m| m.selected) else {
            return Ok(true);
        };
        let on_screen_is_this_size = self.drawn.as_ref().is_some_and(|d| d.size == room);
        if before == selected || self.cell != self.told || !on_screen_is_this_size {
            return Ok(before != selected);
        }
        let (earliest, earliest_first) = self.list_move.map_or((before, first_before), |(_, b, f)| (b, f));
        self.list_move = Some((true, earliest, earliest_first));
        Ok(false)
    }

    /// What is drawn of the screen of the settings: the `rows` lines on the screen (the names of the parts, the settings, and last
    /// where they are saved), the chosen one marked, and under the title what the chosen setting does.
    fn menu_list(&self, rows: usize) -> TocList {
        let Some(menu) = self.menu.as_ref() else {
            return TocList {
                subtitle: String::new(),
                lines: Vec::new(),
                marked: None,
                first: 0,
                total: 0,
            };
        };
        let kept = self.settings.path().map_or_else(
            || "Not saved (--no-state)".to_string(),
            |p| format!("Saved in {}", p.display()),
        );
        let kept = format!("Enter, a click or right changes, left back; Esc closes. {kept}");
        let all: Vec<peekel_core::card::ListLine> = MENU
            .iter()
            .map(|line| match line {
                MenuLine::Part(name) => peekel_core::card::ListLine {
                    depth: 0,
                    text: (*name).to_string(),
                    right: String::new(),
                    kind: peekel_core::card::LineKind::Heading,
                },
                MenuLine::Row(setting) => {
                    let (label, value) = self.setting_row(*setting);
                    peekel_core::card::ListLine {
                        depth: 1,
                        text: label.to_string(),
                        right: value,
                        kind: peekel_core::card::LineKind::Item,
                    }
                }
            })
            .chain(std::iter::once(peekel_core::card::ListLine {
                depth: 0,
                text: plain(&kept),
                right: String::new(),
                kind: peekel_core::card::LineKind::Note,
            }))
            .collect();
        let total = all.len();
        let hint = match MENU.get(menu.selected) {
            Some(MenuLine::Row(setting)) => setting_hint(*setting),
            _ => "",
        };
        TocList {
            subtitle: hint.to_string(),
            lines: all.into_iter().skip(menu.first).take(rows).collect(),
            marked: menu.selected.checked_sub(menu.first),
            first: menu.first,
            total,
        }
    }

    /// Draws what changed of a list, row by row, each as a picture of its own put at its line: the heading, and the two rows between
    /// which the chosen one moved (from `before` to `selected`) or, when the list `scrolled`, all the rows and the bar at the right. A row
    /// is whole cells high, so one that was made once is kept (`row_cache`) and put on the screen again for nothing; a list that scrolls
    /// makes only the rows that come into view. Whether the whole picture has to be drawn after all (a picture could not be made).
    #[allow(clippy::too_many_arguments)]
    fn list_strips(
        &mut self,
        title: &str,
        list: &TocList,
        before: usize,
        selected: usize,
        scroll: isize,
        shape: peekel_core::card::ListShape,
        room: (usize, usize),
    ) -> Result<bool, String> {
        const KEPT_MAX: usize = 800;
        let scrolled = scroll != 0;
        let cell = self.cell_pair();
        let top = shape.top.round() as usize;
        let pitch = shape.pitch.round() as usize;
        let bar_width = peekel_core::card::list_bar_width(room.0, cell.0);
        let background = self.background;
        // what was kept is for this window, this cell and this background only
        let made_for = (room, cell, background);
        if self.row_cache.made_for != Some(made_for) || self.row_cache.rows.len() > KEPT_MAX {
            self.row_cache.rows.clear();
            self.row_cache.made_for = Some(made_for);
        }
        let mut times = [0u128; 4];
        let mut out = vec![BEGIN_FRAME.to_string()];
        // the heading changes every time (the number of the line): it is made again
        let Some(heading) = strip_sixel(
            || {
                peekel_core::card::render_list_part(
                    title,
                    &list.subtitle,
                    &list.lines,
                    list.marked,
                    list.first,
                    list.total,
                    room.0,
                    room.1,
                    cell,
                    background,
                    0,
                    top,
                )
                .ok()
                .map(Image::new)
            },
            (room.0, top),
            background,
            &mut times,
        ) else {
            return Ok(true);
        };
        out.push(format!("\x1b[1;1H{heading}"));
        let widest = peekel_core::card::list_widest(&list.lines);
        let shown = list.lines.len();
        // the terminal moves the rows already on the screen (Windows Terminal moves a Sixel with the text of a scrolled region: tried
        // with CSI S, T, M and L); then only the rows that came into view, and those whose mark changed, are written
        let by_terminal = scrolled && scroll.unsigned_abs() < shown && shown == shape.rows && terminal_moves_pictures();
        let wanted: Vec<usize> = if by_terminal {
            let lines_per_row = pitch / cell.1;
            let first_line = 1 + top / cell.1;
            let last_line = first_line + shown * lines_per_row - 1;
            let step = scroll.unsigned_abs() * lines_per_row;
            out.push(format!(
                "\x1b[{first_line};{last_line}r\x1b[{step}{}\x1b[r",
                if scroll > 0 { 'S' } else { 'T' }
            ));
            let came: Vec<usize> = if scroll > 0 {
                (shown - scroll.unsigned_abs()..shown).collect()
            } else {
                (0..scroll.unsigned_abs()).collect()
            };
            let mut rows: Vec<usize> = [before, selected]
                .iter()
                .filter_map(|line| line.checked_sub(list.first))
                .filter(|at| *at < shown)
                .chain(came)
                .collect();
            rows.sort_unstable();
            rows.dedup();
            rows
        } else if scrolled {
            (0..shown).collect()
        } else {
            let mut two: Vec<usize> = [before, selected]
                .iter()
                .filter_map(|line| line.checked_sub(list.first))
                .filter(|at| *at < list.lines.len())
                .collect();
            two.dedup();
            two
        };
        let (mut made, mut kept) = (0usize, 0usize);
        for at in wanted {
            let line = &list.lines[at];
            let marked = list.marked == Some(at);
            // a heading is drawn otherwise than a line of the same text and depth
            let depth = line.depth
                + usize::from(line.kind != peekel_core::card::LineKind::Item)
                    * 1000
                    * (1 + usize::from(line.kind == peekel_core::card::LineKind::Note));
            let key = (line.text.clone(), line.right.clone(), depth, marked, widest);
            if !self.row_cache.rows.contains_key(&key) {
                let Some(stream) = strip_sixel(
                    || {
                        peekel_core::card::render_list_row(line, marked, widest, room.0, room.1, cell, background)
                            .ok()
                            .map(Image::new)
                    },
                    (room.0 - bar_width.min(room.0), pitch),
                    background,
                    &mut times,
                ) else {
                    return Ok(true);
                };
                self.row_cache.rows.insert(key.clone(), stream);
                made += 1;
            } else {
                kept += 1;
            }
            let Some(stream) = self.row_cache.rows.get(&key) else {
                return Ok(true);
            };
            out.push(format!("\x1b[{};1H{stream}", 1 + (top + at * pitch) / cell.1));
        }
        if scrolled && list.total > shape.rows {
            let Some(stream) = strip_sixel(
                || {
                    peekel_core::card::render_list_bar(
                        list.first,
                        list.lines.len(),
                        list.total,
                        room.0,
                        room.1,
                        cell,
                        background,
                    )
                    .ok()
                    .map(Image::new)
                },
                (bar_width, shape.rows * pitch),
                background,
                &mut times,
            ) else {
                return Ok(true);
            };
            out.push(format!(
                "\x1b[{};{}H{stream}",
                1 + top / cell.1,
                1 + (room.0 - bar_width.min(room.0)) / cell.0
            ));
        }
        out.push(END_FRAME.to_string());
        let tracing = self.trace.is_some();
        if tracing {
            out.push(STATUS_QUESTION.to_string());
        }
        let bytes: usize = out.iter().map(String::len).sum();
        let parts: Vec<&str> = out.iter().map(String::as_str).collect();
        let started = Instant::now();
        write_out(&parts).map_err(|e| write_failed(&e))?;
        if tracing {
            let [t_made, t_scaled, t_quantized, t_encoded] = times;
            self.trace_wait = Some((
                started,
                format!(
                    "strips of the list, {bytes} bytes of Sixel; {made} rows made, {kept} kept{}; drawn in {t_made} ms, scaled in {t_scaled} ms, quantized in {t_quantized} ms, encoded in {t_encoded} ms; written in {} ms",
                    if by_terminal {
                        ", the rows moved by the terminal"
                    } else {
                        ""
                    },
                    started.elapsed().as_millis()
                ),
            ));
        }
        Ok(false)
    }
    /// A key while the list of contents is shown: up and down choose a line, Page Up and Page Down a screenful, Home and End the first
    /// and the last, Enter goes to the page, Esc and `c` close it. Whether the screen has to be drawn again.
    fn toc_key(&mut self, key: &[u8]) -> Result<bool, String> {
        let rows = self.toc_rows();
        let Some(toc) = self.toc.as_mut() else {
            return Ok(false);
        };
        let last = toc.items.len().saturating_sub(1);
        match key {
            b"\r" | b"\n" => self.close_contents(true),
            b"\x1b" | b"\x03" | b"q" | b"Q" | b"c" | b"C" => self.close_contents(false),
            _ => {
                let (before, first_before) = (toc.selected, toc.first);
                match action(key) {
                    Some(Action::Pan(_, dy)) if dy != 0 => {
                        toc.selected = toc.selected.saturating_add_signed(dy.signum() as isize).min(last);
                    }
                    Some(Action::PageDown) => toc.selected = (toc.selected + rows.saturating_sub(1).max(1)).min(last),
                    Some(Action::PageUp) => toc.selected = toc.selected.saturating_sub(rows.saturating_sub(1).max(1)),
                    Some(Action::BookStart | Action::First) => toc.selected = 0,
                    Some(Action::BookEnd | Action::Last) => toc.selected = last,
                    _ => {}
                }
                if toc.selected == before {
                    return Ok(false);
                }
                self.toc_moved(before, first_before)
            }
        }
    }

    /// The mouse while the list of contents is shown: the wheel moves the chosen line, a click on a line goes to its page.
    fn toc_mouse(&mut self, button: u32, row: u32, press: bool) -> Result<bool, String> {
        const WHEEL: u32 = 64;
        let room = self.room();
        let shape = self.list_shape_in(room);
        let cell_height = self.cell.height as f32;
        let Some(toc) = self.toc.as_mut() else {
            return Ok(false);
        };
        let last = toc.items.len().saturating_sub(1);
        match (button, press) {
            (WHEEL, _) | (65, _) => {
                let (before, first_before) = (toc.selected, toc.first);
                toc.selected = if button == WHEEL {
                    toc.selected.saturating_sub(3)
                } else {
                    (toc.selected + 3).min(last)
                };
                if toc.selected == before {
                    return Ok(false);
                }
                self.toc_moved(before, first_before)
            }
            (0, true) => {
                // the picture is as large as the room, so its pixels are those of the window (rows count from 1)
                let y = (row.saturating_sub(1)) as f32 * cell_height + cell_height / 2.0;
                match peekel_core::card::list_row_at(shape, y) {
                    Some(at) if toc.first + at <= last => {
                        toc.selected = toc.first + at;
                        self.close_contents(true)
                    }
                    _ => Ok(false),
                }
            }
            _ => Ok(false),
        }
    }

    /// Looks for `needle` in the text of the book on the screen and goes to the first spread after this one that has it. An empty
    /// word forgets the last search.
    fn search(&mut self, needle: &str) -> Result<(), String> {
        let needle = needle.trim();
        if needle.is_empty() {
            self.message = Some("search forgotten".to_string());
            return self.set_find(None);
        }
        let Some(path) = self.files.get(self.index).cloned() else {
            return Ok(());
        };
        if self.texts.as_ref().is_none_or(|(p, _)| *p != path) {
            self.say("reading the text…")?;
            match page_texts(&path) {
                Ok(Some(texts)) => self.texts = Some((path, find::Index::new(&texts))),
                Ok(None) => {
                    self.message = Some(
                        if peekel_core::djvu::is_djvu(&path) {
                            "this book has no text layer"
                        } else if peekel_core::pdf::is_pdf(&path) {
                            "this PDF has no text to search (a scan)"
                        } else {
                            "only PDF, DjVu, FB2, EPUB, HTML and Word can be searched"
                        }
                        .to_string(),
                    );
                    return Ok(());
                }
                Err(e) => {
                    self.message = Some(plain(&format!("the text cannot be read: {e}")));
                    return Ok(());
                }
            }
        }
        let hits = self
            .texts
            .as_ref()
            .map(|(_, index)| index.pages_with(needle))
            .unwrap_or_default();
        if hits.is_empty() {
            self.message = Some(plain(&format!("\"{needle}\": not found")));
            return self.set_find(None);
        }
        let shown = self.page..self.page + self.shown;
        let only_here = hits.iter().all(|p| shown.contains(p));
        self.set_find(Some(Find {
            needle: plain(needle),
            hits,
            at: None,
        }))?;
        if only_here {
            // nowhere to go: the pages that are shown are read again, with the word marked on them
            self.message = Some("the word is only on the pages that are shown".to_string());
            return self.go_page(self.page);
        }
        self.go_to_hit(true)
    }

    /// Takes a search as the one to show (or none): the pages that were read ahead are dropped, to be read again with the words of the
    /// new search marked. When the page on the screen had marks of the old search, it is read again without them.
    fn set_find(&mut self, find: Option<Find>) -> Result<(), String> {
        let shown = self.page..self.page + self.shown;
        let had_marks = self
            .find
            .as_ref()
            .is_some_and(|f| f.hits.iter().any(|p| shown.contains(p)));
        self.find = find;
        self.preload = None;
        self.turned += 1;
        if had_marks {
            self.go_page(self.page)?;
        }
        Ok(())
    }

    /// Goes to the next (or the previous) spread that has the word of the last search, going round at the ends.
    fn go_to_hit(&mut self, forward: bool) -> Result<(), String> {
        let Some(find) = self.find.as_mut() else {
            return Ok(());
        };
        let spread = self.spread;
        let next = find::next_hit(&find.hits, find.at, self.page, forward, |p| spread.start_of(p));
        match next {
            Some(i) => {
                find.at = Some(i);
                let page = find.hits.get(i).copied().unwrap_or(0);
                self.go_page(spread.start_of(page))
            }
            None => {
                self.message = Some("the word is only on the pages that are shown".to_string());
                Ok(())
            }
        }
    }

    /// Sets the size of the text of the books whose text flows (in per cent, kept within `TEXT_SIZES`) and writes it in the settings; the
    /// book on the screen, if it is one, is laid out again and opened where the reader was in its text. Any other file is not touched:
    /// the size holds for the next such book.
    fn set_text_size(&mut self, percent: u32) -> Result<(), String> {
        let (low, high) = peekel_core::book::TEXT_SIZES;
        let percent = percent.clamp(low, high);
        if percent == self.settings.text_size {
            self.message = Some(format!(
                "the text is at its {} ({percent} %)",
                if percent == high { "largest" } else { "smallest" }
            ));
            return Ok(());
        }
        self.lay_out_again(
            |viewer| {
                peekel_core::book::set_text_size(percent);
                viewer.settings.text_size = percent;
            },
            format!("the text of books at {percent} %"),
            &format!("laying the book out at {percent} %…"),
        )
    }

    /// Changes how books whose text flows are laid out (`change`), writes the settings and says `said`; the book on the screen, if it
    /// is one, is laid out again (saying `laying` meanwhile) and opened where the reader was in its text. Any other file is not
    /// touched: the change holds for the next such book.
    fn lay_out_again(&mut self, change: impl FnOnce(&mut Self), said: String, laying: &str) -> Result<(), String> {
        let path = self.files.get(self.index).cloned();
        let book = path.filter(|p| !self.card && self.pages > 0 && text_flows(p));
        // the place is taken as the book is laid out now
        let place = book.as_ref().and_then(|p| place_of(p, self.page).ok().flatten());
        change(self);
        self.settings.save();
        self.message = Some(said);
        // what was read ahead or kept was laid out the old way, and so were the pages of a search
        self.preload = None;
        self.behind = None;
        self.texts = None;
        self.find = None;
        self.toc_choice = None;
        self.turned += 1;
        let Some(path) = book else {
            return Ok(());
        };
        self.say(laying)?;
        let page = place
            .and_then(|place| page_at(&path, place).ok().flatten())
            .unwrap_or(self.page);
        self.asked = Instant::now();
        self.pending = self.take_read(self.index, self.spread.start_of(page));
        Ok(())
    }

    /// Turns one of the switches of the look of books whose text flows (`Setting::Justify`, `MarkParagraphs`, `ChosenSize`,
    /// `CodeBackdrop`) the other way; the book on the screen is laid out again.
    fn set_book_look(&mut self, setting: Setting) -> Result<(), String> {
        let s = &self.settings;
        let (on, name) = match setting {
            Setting::Justify => (!s.justify, "paragraphs justified"),
            Setting::MarkParagraphs => (!s.mark_paragraphs, "paragraphs always set apart"),
            Setting::ChosenSize => (!s.chosen_size, "the text at the size chosen"),
            Setting::CodeBackdrop => (!s.code_backdrop, "code on a shade"),
            _ => return Ok(()),
        };
        self.lay_out_again(
            |viewer| {
                match setting {
                    Setting::Justify => viewer.settings.justify = on,
                    Setting::MarkParagraphs => viewer.settings.mark_paragraphs = on,
                    Setting::ChosenSize => viewer.settings.chosen_size = on,
                    _ => viewer.settings.code_backdrop = on,
                }
                apply_book_look(&viewer.settings);
            },
            format!("{name}: {}", if on { "yes" } else { "no" }),
            "laying the book out again…",
        )
    }

    /// Makes the small pages of DjVu books as DjVuLibre makes them, or our way, and writes it in the settings; the book on the screen,
    /// if it is one, is read again, its zoom and place kept.
    fn set_scans_as_djvulibre(&mut self, on: bool) {
        self.settings.scans_as_djvulibre = on;
        apply_scans_look(&self.settings);
        self.settings.save();
        self.info = None;
        self.message = Some(format!(
            "DjVu: small pages {}",
            if on { "as DjVuLibre makes them" } else { "made our way" }
        ));
        // what was read ahead or kept was made the other way
        self.preload = None;
        self.behind = None;
        self.turned += 1;
        if self
            .files
            .get(self.index)
            .is_some_and(|p| !self.card && peekel_core::djvu::is_djvu(p))
        {
            self.asked = Instant::now();
            self.pending = self.take_read(self.index, self.spread.start_of(self.page));
        }
    }

    /// Sets the spread up another way and reads the pages again, the same first page (or the first of its pair) kept.
    fn set_spread(&mut self, spread: Spread) -> Result<(), String> {
        self.spread = spread;
        self.kept = None;
        self.preload = None;
        self.turned += 1;
        let start = spread.start_of(self.page);
        self.go_page(start)
    }

    /// Draws the pages of DjVu books as `scans` says (`v`, the settings) and writes it in the settings; the book on the screen, if it
    /// is one, is read again, its zoom and place kept (the page is as large to the view either way).
    fn set_scans(&mut self, scans: Scans) {
        self.settings.scans = scans;
        self.settings.save();
        self.info = None;
        // short: the line at the bottom says the way after the size of the page all the time
        self.message = Some(format!("DjVu: {} (v: {})", scans.word(), scans.next().word()));
        // what was read ahead or kept was made with the other pixels
        self.preload = None;
        self.behind = None;
        self.turned += 1;
        if self
            .files
            .get(self.index)
            .is_some_and(|p| !self.card && peekel_core::djvu::is_djvu(p))
        {
            self.asked = Instant::now();
            self.pending = self.take_read(self.index, self.spread.start_of(self.page));
        }
    }

    /// Turns the night look of books on or off and writes it in the settings; the book on the screen, if it is one, is read again in
    /// the new look, the same pages kept. A picture or a card is not touched: the look holds for the next book.
    fn set_night(&mut self, on: bool) -> Result<(), String> {
        self.settings.night = on;
        self.settings.save();
        self.info = None;
        self.message = Some(
            if on {
                "night: the pages of books dark (b turns it off)"
            } else {
                "day: the pages of books as they are (b: night)"
            }
            .to_string(),
        );
        // what was read ahead or kept was made in the other look
        self.kept = None;
        self.preload = None;
        self.behind = None;
        self.turned += 1;
        let book = self.files.get(self.index).is_some_and(|p| !self.card && is_book(p));
        if !book {
            return Ok(());
        }
        self.asked = Instant::now();
        self.pending = self.take_read(self.index, self.spread.start_of(self.page));
        Ok(())
    }
}

/// Tells the core how the settings say books whose text flows are laid out (the next book or page laid out follows it).
fn apply_book_look(settings: &Settings) {
    peekel_core::book::set_justify(settings.justify);
    peekel_core::book::set_mark_paragraphs(settings.mark_paragraphs);
    peekel_core::book::set_normalize_text(settings.chosen_size);
    peekel_core::book::set_code_backdrop(settings.code_backdrop);
}

/// Tells the core how the settings say a small page of a DjVu book is made: both its background and its letters as DjVuLibre makes
/// them, or both our way (the core keeps the two apart for measuring; a reader has one choice).
fn apply_scans_look(settings: &Settings) {
    peekel_core::djvu::set_shrink_as_djvulibre(settings.scans_as_djvulibre);
    peekel_core::djvu::set_mask_as_djvulibre(settings.scans_as_djvulibre);
}

/// The alternate screen for as long as this lives; leaving it (and showing the cursor) also happens when a panic unwinds.
struct AltScreen;

impl AltScreen {
    fn enter() -> Result<Self, String> {
        write_out(&[ENTER_SCREEN]).map_err(|e| write_failed(&e))?;
        Ok(Self)
    }
}

/// Windows Terminal's key mode (9001) switched on for as long as this lives, if the terminal knows it: each key arrives as a record that
/// tells the key pressed (see `peekel_term::win32keys`). Given back when this is dropped.
struct KeyRecords(bool);

impl KeyRecords {
    fn enable(known: bool) -> Self {
        if known {
            let _ = write_out(&["\x1b[?9001h"]);
        }
        Self(known)
    }
}

impl Drop for KeyRecords {
    fn drop(&mut self) {
        if self.0 {
            let _ = write_out(&["\x1b[?9001l"]);
        }
    }
}

impl Drop for AltScreen {
    fn drop(&mut self) {
        let _ = write_out(&[LEAVE_SCREEN]);
    }
}

/// Shows the picture until it is closed; what the first drawing cost. With `timing` the terminal is also asked when it
/// has dealt with the picture.
fn show(path: &Path, options: Options) -> Result<Report, String> {
    // declared in this order so that the screen is given back before the terminal's mode is
    let _raw = RawMode::enter().map_err(|e| format!("cannot set up the terminal: {e}"))?;
    let keys = input::spawn_timed();
    let mut parser = Parser::default();
    let mut decoder = KeyDecoder::default();

    let asked = Instant::now();
    write_out(&[QUESTIONS]).map_err(|e| write_failed(&e))?;
    let found = probe(&keys, &mut parser);
    let probe_time = asked.elapsed();
    let _ = WINDOWS_TERMINAL.set(std::env::var_os("WT_SESSION").is_some() && found.key_records);
    if found.sixel == Some(false) && !options.force {
        let said: Vec<String> = found.attributes.iter().map(u32::to_string).collect();
        return Err(format!(
            "this terminal does not say it can show Sixel graphics: it answered \"{}\" to the question what it is, and 4 \
             among the numbers would mean Sixel. If it shows Sixel and does not say so, or this is tmux or screen, \
             --force draws anyway",
            said.join(";")
        ));
    }
    let window = window_size().map_err(|e| format!("cannot read the window's size: {e}"))?;
    let told = found.cell.or_else(|| cell_from_window(window)).unwrap_or(DEFAULT_CELL);
    let cell = real_cell(told);

    // read and drawn before the screen is taken: a file that cannot be read is reported on the screen the user came from
    let dir = state::default_dir().filter(|_| options.remember);
    let state = dir.clone().map_or_else(State::none, State::open);
    let settings = dir.map_or_else(Settings::none, Settings::open);
    // the files of the folder that Space and Backspace go through
    let (files, index) = files::in_folder(path, listing(&options, &settings));
    // the user's own fonts are looked in first when text is drawn (a card, a page of Word)
    peekel_core::fontset::set_user_dirs(settings.font_dirs());
    // where the book was left, how it was put together and turned
    let remembered = std::fs::metadata(path)
        .ok()
        .and_then(|m| state.get(&state::key_of(path), m.len()).cloned());
    let own = remembered.as_ref().map_or_else(BookChoice::default, |book| BookChoice {
        count: book.count,
        equal_height: book.equal_height,
        cover_alone: book.cover_alone,
    });
    let spread = settings.spread(own);
    // a book whose text flows is laid out at the size of text chosen; left at another size, its page is found again from the place
    peekel_core::book::set_text_size(settings.text_size);
    apply_book_look(&settings);
    apply_scans_look(&settings);
    let (start, quarters) = match &remembered {
        Some(book) => {
            let start = spread.start_of(page_of(path, book));
            let turn_of = |page: usize| {
                let own = book.turns.iter().find(|(p, _)| *p == page).map_or(0, |(_, q)| *q);
                (own + book.file_turn).rem_euclid(4)
            };
            (start, (0..spread.count).map(|i| turn_of(start + i)).collect())
        }
        None => (0, Vec::new()),
    };
    let reading = Instant::now();
    let Loaded {
        image,
        shown,
        pages,
        card,
        note,
    } = match load_spread(
        path,
        start,
        spread,
        &quarters,
        listing(&options, &settings) == Listing::Every,
        None,
        settings.night,
        detail_of(settings.scans, room(window, cell)),
    ) {
        // a file that is no picture is passed over without a word (the cards are an option)
        Err(DecodeError::NotAPicture) => return Ok(Report::default()),
        other => other.map_err(|e| format!("{}: {e}", path.display()))?,
    };
    let reading = reading.elapsed();
    let start = if card {
        0
    } else {
        spread.start_of(start.min(pages.saturating_sub(1)))
    };
    let kept = remembered.as_ref().and_then(|b| b.kept).filter(|_| !card);
    let view = match kept {
        Some(relative) => View::from_relative(relative, image.size(), room(window, cell)),
        None => View::new(image.size()),
    };
    let mut viewer = Viewer {
        files,
        index,
        view,
        image,
        page: start,
        pages,
        shown,
        spread,
        info: None,
        show_info: card,
        help: false,
        drag: None,
        card,
        window,
        cell,
        told,
        background: found.background.unwrap_or([0, 0, 0]),
        options,
        message: note.map(|n| plain(&n)),
        drawn: None,
        preload: None,
        ready: None,
        behind: None,
        on_screen: None,
        pending: None,
        forward: true,
        steady: None,
        turns: std::collections::HashMap::new(),
        file_turns: std::collections::HashMap::new(),
        turned: 0,
        kept,
        state,
        own: std::collections::HashMap::new(),
        settings,
        adopted: std::collections::HashSet::new(),
        prompt: None,
        asking_page: false,
        find: None,
        toc: None,
        menu: None,
        toc_choice: None,
        quiet_message: false,
        trace: std::env::var_os("PEEKEL_TRACE")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from),
        trace_start: Instant::now(),
        asked: Instant::now(),
        trace_wait: None,
        list_move: None,
        row_cache: RowCache::default(),
        texts: None,
    };
    viewer.adopt(index);
    let rendered = viewer.frame(None)?;
    let _key_records = KeyRecords::enable(found.key_records);
    let _screen = AltScreen::enter()?;
    let (painting_began, written) = viewer.paint(&rendered, true, options.timing)?;
    viewer.read_ahead();
    let mut report = Report {
        probe: probe_time,
        decode: reading + rendered.scaling,
        quantize: rendered.quantizing,
        encode: rendered.encoding,
        paint: written,
        processed: None,
        size: rendered.size,
        stream_bytes: rendered.stream.len(),
        redraws: Vec::new(),
        cell: layout::cell_report(told, viewer.cell),
    };
    let mut awaiting_status = options.timing;
    let coarse_moves = std::env::var("PEEKEL_MOVE_COARSE").map_or(true, |v| v != "0");
    let move_dither = std::env::var("PEEKEL_MOVE_DITHER")
        .ok()
        .and_then(|v| v.parse::<i32>().ok())
        .unwrap_or(MOVE_DITHER);
    // when the picture drawn coarsely, while it was being moved, is to be drawn in full
    let mut settle: Option<Instant> = None;

    loop {
        // What came in since the last look: the keys, the terminal's answers; a burst of key presses (a held arrow) is
        // dealt with together, so that the picture is drawn once for all of them.
        let mut events = Vec::new();
        match keys.recv_timeout(TICK) {
            Ok((byte, at)) => {
                // the times of the bytes tell Escape and a key pressed after it from Alt and the key, when a busy loop reads them together
                events.extend(parser.push_at(byte, at, TICK));
                while let Ok((byte, at)) = keys.try_recv() {
                    events.extend(parser.push_at(byte, at, TICK));
                }
            }
            Err(RecvTimeoutError::Timeout) => events.extend(parser.flush()),
            Err(RecvTimeoutError::Disconnected) => return Ok(report),
        }

        let mut changed = false;
        let (view_before, index_before, page_before) = (viewer.view, viewer.index, viewer.page);
        let mut resized = false;
        for event in events {
            match event {
                Event::StatusReport if awaiting_status => {
                    report.processed = Some(painting_began.elapsed());
                    awaiting_status = false;
                }
                Event::StatusReport => {
                    if let Some((began, what)) = viewer.trace_wait.take() {
                        viewer.trace_line(&format!(
                            "{what}; the terminal was done {} ms after writing began",
                            began.elapsed().as_millis()
                        ));
                    }
                }
                Event::Key(bytes) => {
                    // the key as it is for the viewer (a letter of the Russian layout is the key it sits on, a record of Windows Terminal's
                    // key mode is the key it says); nothing yet for half a letter, a key let go, or Shift held alone
                    let Some(pressed) = decoder.feed(&bytes) else {
                        continue;
                    };
                    viewer.trace_line(&format!("key {:?}", String::from_utf8_lossy(&pressed.key)));
                    // a word is being typed: the keys are its letters
                    if viewer.prompt.is_some() && !options.any_key {
                        changed |= viewer.type_key(&pressed.typed)?;
                        continue;
                    }
                    // the settings are shown: the keys choose a row and change it
                    if viewer.menu.is_some() && !options.any_key {
                        changed |= viewer.menu_key(&pressed.key)?;
                        continue;
                    }
                    // the list of contents is shown: the keys choose a line
                    if viewer.toc.is_some() && !options.any_key {
                        changed |= viewer.toc_key(&pressed.key)?;
                        continue;
                    }
                    let bytes = pressed.key;
                    let action = if options.any_key {
                        Some(Action::Quit)
                    } else {
                        action(&bytes)
                    };
                    match action {
                        // while the list of keys is shown, Esc and q take it away, as any other key does
                        Some(Action::Quit) if !viewer.help => {
                            if !options.any_key {
                                viewer.remember();
                            }
                            return Ok(report);
                        }
                        Some(other) => changed |= viewer.apply(other)?,
                        // a key that means nothing takes the list of keys away too
                        None if viewer.help => changed |= viewer.close_help(),
                        None => {}
                    }
                }
                Event::Mouse {
                    button,
                    column,
                    row,
                    press,
                } if !options.any_key => changed |= viewer.mouse(button, column, row, press)?,
                _ => {}
            }
        }

        // the strips of a list whose row moved: once for the whole burst of keys, unless the whole picture is drawn anyway
        if changed {
            viewer.list_move = None;
        } else {
            changed |= viewer.flush_list_move()?;
        }

        // a picture asked for that has been read since
        changed |= viewer.poll_pending();

        // the window's size is looked at while nothing is pressed
        if let Ok(now) = window_size()
            && now != viewer.window
        {
            viewer.window = now;
            let (size, room) = (viewer.image.size(), viewer.room());
            viewer.view.pan(0.0, 0.0, size, room);
            viewer.drawn = None;
            changed = true;
            resized = true;
        }

        // Only the view moved (a move or a zoom): a coarse frame now, which is quick, and the full one when it has stopped.
        let moved = changed
            && !resized
            && coarse_moves
            && viewer.view != view_before
            && viewer.index == index_before
            && viewer.page == page_before
            && !viewer.show_info
            && !viewer.help
            && viewer.pending.is_none();
        let settled = !changed && settle.is_some_and(|at| Instant::now() >= at);
        if changed || settled {
            let coarse = moved.then_some(move_dither);
            let rendered = viewer.frame(coarse)?;
            let (_, written) = viewer.paint(&rendered, false, false)?;
            report.redraws.push((
                rendered.scaling + rendered.quantizing + rendered.encoding,
                written,
                rendered.rough,
            ));
            settle = rendered.rough.then(|| Instant::now() + SETTLE);
            viewer.keep_frame(rendered);
        }
    }
}

/// The page (from 0) that the number typed after `p` names, in a file of `pages` pages: as the line at the bottom counts them (from 1);
/// past the last page, the last. None for nothing typed or no number.
fn page_of_number(typed: &str, pages: usize) -> Option<usize> {
    let number: usize = typed.trim().parse().ok()?;
    (pages > 0).then(|| number.clamp(1, pages) - 1)
}

/// What to read ahead of the key, as (file, page): on page `page` of `pages` of the file `index` of `count`, moving `forward` or
/// back. A file of many pages has its next page (or the one before), until its end is reached; then, and for a file of one page, the
/// neighbouring file's first page. None when there is nothing else to read.
fn ahead(index: usize, count: usize, page: usize, pages: usize, forward: bool) -> Option<(usize, usize)> {
    if pages > 1 {
        let next = if forward {
            page.checked_add(1)
        } else {
            page.checked_sub(1)
        };
        if let Some(next) = next.filter(|&n| n < pages) {
            return Some((index, next));
        }
    }
    if count < 2 {
        return None;
    }
    let neighbour = if forward {
        (index + 1) % count
    } else {
        (index + count - 1) % count
    };
    Some((neighbour, 0))
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its file should stop at once
mod tests {
    use super::{
        DecodeError, MENU, MenuLine, Setting, ahead, chapter_at, load, menu_end, menu_next, page_of_number, plain,
    };

    #[test]
    fn every_setting_is_on_the_screen_of_the_settings_once_and_the_keys_reach_it_past_the_names_of_the_parts() {
        let all = [
            Setting::TextSize,
            Setting::Justify,
            Setting::MarkParagraphs,
            Setting::ChosenSize,
            Setting::CodeBackdrop,
            Setting::Night,
            Setting::Scope,
            Setting::Pages,
            Setting::EqualHeight,
            Setting::CoverAlone,
            Setting::Scans,
            Setting::ScansAsDjvulibre,
            Setting::Browse,
        ];
        for setting in all {
            let n = MENU.iter().filter(|l| **l == MenuLine::Row(setting)).count();
            assert_eq!(n, 1, "{setting:?}");
        }
        // down from the first setting with the keys meets every setting, in order, never the name of a part, and stops at the last
        let rows: Vec<usize> = (0..MENU.len())
            .filter(|&i| matches!(MENU[i], MenuLine::Row(_)))
            .collect();
        let mut met = vec![menu_end(true)];
        while let Some(&at) = met.last() {
            let next = menu_next(at, true);
            if next == at {
                break;
            }
            met.push(next);
        }
        assert_eq!(met, rows);
        assert_eq!(menu_end(false), *rows.last().expect("rows"));
        // up from the first stays; up from the first setting of a part goes over its name to the last setting of the part before
        assert_eq!(menu_next(rows[0], false), rows[0]);
        assert!(
            matches!(MENU[0], MenuLine::Part(_)),
            "the list begins with the name of its first part"
        );
        let scans = MENU
            .iter()
            .position(|l| *l == MenuLine::Row(Setting::Scans))
            .expect("scans");
        let cover = MENU
            .iter()
            .position(|l| *l == MenuLine::Row(Setting::CoverAlone))
            .expect("cover");
        assert_eq!(menu_next(scans, false), cover);
        assert_eq!(menu_next(cover, true), scans);
    }

    #[test]
    fn the_number_typed_after_p_is_a_page_counted_from_one_and_kept_in_the_book() {
        assert_eq!(page_of_number("1", 300), Some(0));
        assert_eq!(page_of_number("250", 300), Some(249));
        assert_eq!(page_of_number(" 300 ", 300), Some(299));
        assert_eq!(page_of_number("9999999", 300), Some(299), "past the end: the last page");
        assert_eq!(page_of_number("0", 300), Some(0));
        assert_eq!(page_of_number("", 300), None);
        assert_eq!(page_of_number("12a", 300), None);
        assert_eq!(page_of_number("5", 0), None);
    }

    #[test]
    fn a_scan_read_for_a_small_window_is_looked_at_as_the_whole_page_and_read_again_whole_when_wanted() {
        use super::{Detail, load_spread};
        use crate::spread::Spread;
        use peekel_core::view::{View, render};
        // the test page of the core: 72 x 72, its background 24 x 24
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/tests/djvu/ramp.djvu");
        let one = Spread {
            count: 1,
            ..Spread::DEFAULT
        };
        let read =
            |detail, quarters: &[i32]| load_spread(&path, 0, one, quarters, false, None, false, detail).expect("reads");
        let mut whole = read(Detail::Whole, &[0]);
        let mut small = read(Detail::Room((18, 18), 1.0), &[0]);
        assert_eq!(small.image.size(), whole.image.size(), "the same picture to the view");
        assert_eq!((small.pages, small.shown), (1, 1));
        let fit = View::new(whole.image.size());
        assert_eq!(
            small.image.short(&fit, (18, 18)),
            None,
            "enough pixels for the room it was read for"
        );
        assert!(
            small.image.short(&fit, (72, 72)).is_some(),
            "a larger window wants more"
        );
        // the frames for the small room come out the same size, and alike
        let a = render(&mut small.image, &fit, (18, 18)).expect("a frame");
        let b = render(&mut whole.image, &fit, (18, 18)).expect("a frame");
        assert_eq!((a.picture.width, a.picture.height), (b.picture.width, b.picture.height));
        // turned a quarter, the whole size is turned too
        let turned = read(Detail::Room((18, 18), 1.0), &[1]);
        assert_eq!(turned.image.size(), (72, 72));
        // twice as fine: still a smaller copy (36 of 72), never short at the room it was read for
        let finer = read(Detail::Room((9, 9), 2.0), &[0]);
        assert_eq!(finer.image.size(), (72, 72));
        assert_eq!(finer.image.short(&fit, (9, 9)), None);
        assert_eq!(finer.image.short(&fit, (18, 18)), None, "twice the room: still enough");
        // a room larger than half the page, the background (24 x 24) too small to make the page at 60 from: the whole page, as before
        let large = read(Detail::Room((60, 60), 1.0), &[0]);
        assert_eq!(large.image.short(&fit, (72, 72)), None);
        assert_eq!(large.image.size(), (72, 72));
    }

    /// A file of its own in the system's temporary folder, removed by the caller.
    fn file_with(name: &str, content: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("peekel-load-{}-{name}", std::process::id()));
        std::fs::write(&path, content).expect("makes the file");
        path
    }

    #[test]
    fn a_file_that_is_no_picture_gets_a_card_without_a_message_when_cards_are_on() {
        let path = file_with("notes.txt", b"just words");
        let loaded = load(&path, 0, true).expect("a card");
        assert!(loaded.card);
        assert_eq!(loaded.note, None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_file_that_is_no_picture_is_skipped_when_cards_are_off() {
        let path = file_with("other.txt", b"just words");
        let error = load(&path, 0, false).err().expect("not shown");
        assert!(matches!(error, DecodeError::NotAPicture), "{error}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_cut_picture_gets_a_card_and_the_reason_when_cards_are_on_and_the_error_when_off() {
        let path = file_with("cut.png", b"\x89PNG\r\n\x1a\nnot really");
        let loaded = load(&path, 0, true).expect("a card");
        assert!(loaded.card);
        assert!(loaded.note.is_some());
        let error = load(&path, 0, false).err().expect("an error");
        assert!(!matches!(error, DecodeError::NotAPicture), "{error}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_file_that_is_not_there_is_an_error_and_not_a_card() {
        assert!(load(std::path::Path::new("no-such-folder-here/x.png"), 0, true).is_err());
    }

    #[test]
    fn a_name_with_an_escape_sequence_is_printed_without_the_escape() {
        assert_eq!(plain("a\x1b]0;x\x07b"), "a?]0;x?b");
        assert_eq!(plain("photo \u{e9}.jpg"), "photo \u{e9}.jpg");
    }

    #[test]
    fn the_chapter_of_a_page_is_the_line_with_the_greatest_page_not_past_it_and_not_the_last_line_that_is_early_enough()
    {
        use peekel_core::djvu::ContentsItem;
        let item = |page: Option<usize>| ContentsItem {
            depth: 0,
            title: String::new(),
            page,
        };
        // a contents that ends with a line leading back to page 3 (as the author's 10th-form algebra does)
        let items: Vec<ContentsItem> = [7, 7, 15, 32, 39, 188, 270, 3].map(|p| item(Some(p))).to_vec();
        assert_eq!(chapter_at(&items, 38), 3, "page 32, not the last line");
        assert_eq!(chapter_at(&items, 100), 4, "page 39");
        assert_eq!(chapter_at(&items, 188), 5);
        assert_eq!(chapter_at(&items, 7), 1, "the last of the lines of the same page");
        assert_eq!(chapter_at(&items, 0), 0, "before the first line: the first line");
        let none = [item(None), item(Some(5))];
        assert_eq!(chapter_at(&none, 9), 1);
        assert_eq!(chapter_at(&[], 9), 0);
    }
    #[test]
    fn a_file_of_many_pages_reads_its_next_page_ahead_and_a_file_of_one_page_its_neighbour() {
        // page 3 of 10 of file 2 of 5
        assert_eq!(ahead(2, 5, 3, 10, true), Some((2, 4)));
        assert_eq!(ahead(2, 5, 3, 10, false), Some((2, 2)));
        // at the end of the pages the next thing is the neighbouring file, from its first page
        assert_eq!(ahead(2, 5, 9, 10, true), Some((3, 0)));
        assert_eq!(ahead(2, 5, 0, 10, false), Some((1, 0)));
        // a file of one page, and the ends of the folder
        assert_eq!(ahead(0, 5, 0, 1, true), Some((1, 0)));
        assert_eq!(ahead(0, 5, 0, 1, false), Some((4, 0)));
        assert_eq!(ahead(4, 5, 0, 1, true), Some((0, 0)));
        // one file of one page: nothing to read ahead; one file of many pages: its pages
        assert_eq!(ahead(0, 1, 0, 1, true), None);
        assert_eq!(ahead(0, 1, 0, 3, true), Some((0, 1)));
        assert_eq!(ahead(0, 1, 2, 3, true), None);
    }
}
