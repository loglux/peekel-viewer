//! The viewer installs itself (`--install`), sets up the file managers it finds (`--setup mc`, `--setup far`) and takes all of it away
//! again (`--uninstall`). Each step says what it will change and asks; what is changed in a file manager's settings is backed up first.
//!
//! - Midnight Commander reads one extension file: the user's `mc.ext.ini` if there is one, else the system's (never both; read in
//!   mc 4.8.30's `ext.c`), and takes the first section whose name rule matches, with no look further down. So a user file is first
//!   made a copy of the system one, as mc's own "Edit extension file" does, and the section `[Peekel]` goes first, with F3 and Enter
//!   both (the author, 11 November 2026: Enter would else do nothing for these files).
//! - Far keeps its file associations in `associations.db` of its profile. `Far.exe -import` adds what it is given before what is
//!   there and never replaces (tried on a profile of its own: the same file twice gives two of each; an empty list removes nothing),
//!   so the list is read with `-export`, made anew (ours first, every other as it was), the database moved aside as the backup, the
//!   new list imported into an empty one, and read back to be compared; on any difference the backup is put back.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::files;

/// What was asked for on the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Install,
    Uninstall,
    SetupMc,
    SetupFar(Option<PathBuf>),
}

/// Asks yes or no in the console; with `--yes` every answer is yes.
pub struct Asker {
    pub yes: bool,
}

impl Asker {
    fn ask(&self, question: &str) -> bool {
        if self.yes {
            println!("{question} yes");
            return true;
        }
        print!("{question} [Y/n] ");
        let _ = io::stdout().flush();
        let mut answer = String::new();
        match io::stdin().lock().read_line(&mut answer) {
            Ok(0) | Err(_) => {
                println!();
                false
            }
            Ok(_) => {
                let answer = answer.trim().to_lowercase();
                matches!(answer.as_str(), "" | "y" | "yes" | "д" | "да")
            }
        }
    }
}

/// Runs the action; the error is what to say.
pub fn run(action: &Action, asker: &Asker) -> Result<(), String> {
    match action {
        Action::Install => install(asker),
        Action::Uninstall => uninstall(asker),
        Action::SetupMc => setup_mc(&command_of(&installed_or_this()), asker),
        Action::SetupFar(far) => {
            let far = find_far(far.as_deref())
                .ok_or("Far Manager was not found: give the path of Far.exe after --setup far")?;
            setup_far(&far, &installed_or_this(), asker)
        }
    }
}

// ---------------------------------------------------------------- the program itself

/// The program's name in its folder.
fn exe_name() -> &'static str {
    if cfg!(windows) { "peekel.exe" } else { "peekel" }
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Where the program is installed: `%LOCALAPPDATA%\Programs\peekel` on Windows, `~/.local/bin` elsewhere.
fn install_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        env_dir("LOCALAPPDATA").map(|d| d.join("Programs").join("peekel"))
    } else {
        env_dir("HOME").map(|h| h.join(".local").join("bin"))
    }
}

fn this_exe() -> Option<PathBuf> {
    std::env::current_exe().ok().map(|p| p.canonicalize().unwrap_or(p))
}

/// The installed program if it is there, else this one: what the file managers are told to run.
fn installed_or_this() -> PathBuf {
    install_dir()
        .map(|d| d.join(exe_name()))
        .filter(|p| p.is_file())
        .or_else(this_exe)
        .unwrap_or_else(|| PathBuf::from(exe_name()))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Seconds since 1970, for the names of backups.
fn stamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `path` with `.before-peekel-<seconds>` added: the name of its backup.
fn backup_name(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(format!(".before-peekel-{}", stamp()));
    path.with_file_name(name)
}

fn copy_dir(from: &Path, to: &Path) -> io::Result<usize> {
    std::fs::create_dir_all(to)?;
    let mut count = 0;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            count += copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
            count += 1;
        }
    }
    Ok(count)
}

fn install(asker: &Asker) -> Result<(), String> {
    let dir = install_dir().ok_or("no folder to install to (LOCALAPPDATA or HOME is not set)")?;
    let target = dir.join(exe_name());
    let this = this_exe().ok_or("this program's own path is not known")?;
    let version = env!("CARGO_PKG_VERSION");
    if same_file(&this, &target) {
        println!("peekel {version} is in its place: {}", target.display());
    } else if asker.ask(&format!("Copy peekel {version} to {}?", target.display())) {
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        std::fs::copy(&this, &target).map_err(|e| format!("{}: {e} (is it open?)", target.display()))?;
        println!("  copied");
    } else {
        println!("Nothing is installed.");
        return Ok(());
    }
    if !cfg!(windows)
        && let (Some(fonts), Some(own)) = (peekel_core::fontset::beside_program(), peekel_core::fontset::own_dir())
        && fonts.is_dir()
        && !same_file(&fonts, &own)
        && asker.ask(&format!("Copy the free fonts that came with it to {}?", own.display()))
    {
        let count = copy_dir(&fonts, &own).map_err(|e| format!("{}: {e}", own.display()))?;
        println!("  {count} files copied");
    }
    put_on_path(&dir, asker)?;
    let far = if cfg!(windows) { find_far(None) } else { None };
    if let Some(far) = &far {
        println!("Far Manager: {}", far.display());
        report(setup_far(far, &target, asker));
    }
    let mc = mc_is_there();
    if mc {
        println!("Midnight Commander:");
        report(setup_mc(&command_of(&target), asker));
    }
    if far.is_none() && !mc {
        println!(
            "No Far Manager or Midnight Commander was found: nothing to set up there (`peekel --setup ...` later)."
        );
    }
    println!("Done. `peekel --uninstall` takes it away again.");
    Ok(())
}

fn report(result: Result<(), String>) {
    if let Err(message) = result {
        println!("  not done: {message}");
    }
}

fn uninstall(asker: &Asker) -> Result<(), String> {
    if cfg!(windows)
        && let Some(far) = find_far(None)
    {
        report(unset_far(&far, asker));
    }
    let mc = mc_ext_path();
    if let Some(mc) = mc.filter(|p| p.is_file()) {
        let text = std::fs::read_to_string(&mc).map_err(|e| format!("{}: {e}", mc.display()))?;
        let without = without_mc_section(&text);
        if without != text && asker.ask(&format!("Take the section [Peekel] out of {}?", mc.display())) {
            let backup = backup_name(&mc);
            std::fs::copy(&mc, &backup).map_err(|e| format!("{}: {e}", backup.display()))?;
            std::fs::write(&mc, without).map_err(|e| format!("{}: {e}", mc.display()))?;
            println!("  done; the file as it was: {}", backup.display());
        }
    }
    if !cfg!(windows)
        && let Some(own) = peekel_core::fontset::own_dir().filter(|d| d.is_dir())
        && asker.ask(&format!("Delete the fonts in {}?", own.display()))
    {
        std::fs::remove_dir_all(&own).map_err(|e| format!("{}: {e}", own.display()))?;
        println!("  deleted");
    }
    let Some(dir) = install_dir() else {
        return Ok(());
    };
    take_off_path(&dir, asker)?;
    let target = dir.join(exe_name());
    if target.is_file() && asker.ask(&format!("Delete {}?", target.display())) {
        let running = this_exe().is_some_and(|this| same_file(&this, &target));
        if cfg!(windows) && running {
            // a running program cannot delete itself in Windows: a shell started now does it once this one has closed
            delete_after_exit(&target, &dir)?;
            println!("  it is deleted as soon as this program closes");
        } else {
            std::fs::remove_file(&target).map_err(|e| format!("{}: {e}", target.display()))?;
            if cfg!(windows) {
                let _ = std::fs::remove_dir(&dir);
            }
            println!("  deleted");
        }
    }
    if let Some(state) = crate::state::default_dir().filter(|d| d.is_dir()) {
        println!(
            "What peekel remembers about your books, and its settings, stay in {}.",
            state.display()
        );
    }
    Ok(())
}

#[cfg(windows)]
fn hidden(command: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW)
}

#[cfg(not(windows))]
fn hidden(command: &mut Command) -> &mut Command {
    command
}

fn delete_after_exit(target: &Path, dir: &Path) -> Result<(), String> {
    let script = format!(
        "ping -n 3 127.0.0.1 >nul & del /f /q \"{}\" & rmdir \"{}\"",
        target.display(),
        dir.display()
    );
    hidden(Command::new("cmd").args(["/d", "/c", &script]))
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("cmd: {e}"))
}

// ---------------------------------------------------------------- PATH

/// The user's own PATH of Windows (not the one of this process, which has the system's too).
fn user_path() -> Result<String, String> {
    let out = hidden(Command::new("powershell").args([
        "-NoProfile",
        "-Command",
        "[Environment]::GetEnvironmentVariable('Path','User')",
    ]))
    .output()
    .map_err(|e| format!("powershell: {e}"))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn set_user_path(path: &str) -> Result<(), String> {
    let quoted = path.replace('\'', "''");
    let status = hidden(Command::new("powershell").args([
        "-NoProfile",
        "-Command",
        &format!("[Environment]::SetEnvironmentVariable('Path','{quoted}','User')"),
    ]))
    .status()
    .map_err(|e| format!("powershell: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("PATH could not be set".to_string())
    }
}

fn has_entry(list: &str, dir: &Path, separator: char) -> bool {
    let dir = dir.to_string_lossy();
    let dir = dir.trim_end_matches(['\\', '/']);
    list.split(separator)
        .any(|e| e.trim_end_matches(['\\', '/']).eq_ignore_ascii_case(dir))
}

fn put_on_path(dir: &Path, asker: &Asker) -> Result<(), String> {
    if cfg!(windows) {
        let path = user_path()?;
        if has_entry(&path, dir, ';') {
            return Ok(());
        }
        if asker.ask(&format!(
            "Add {} to your PATH (so that `peekel` runs from any folder)?",
            dir.display()
        )) {
            let new = if path.is_empty() {
                dir.display().to_string()
            } else {
                format!("{};{}", path.trim_end_matches(';'), dir.display())
            };
            set_user_path(&new)?;
            println!("  added; a terminal opened from now on has it");
        }
    } else if !std::env::var("PATH").is_ok_and(|p| has_entry(&p, dir, ':')) {
        println!(
            "{} is not on your PATH: add it in your shell's profile (Ubuntu's ~/.profile adds it at the next login when the folder is there).",
            dir.display()
        );
    }
    Ok(())
}

fn take_off_path(dir: &Path, asker: &Asker) -> Result<(), String> {
    if !cfg!(windows) {
        return Ok(());
    }
    let path = user_path()?;
    if !has_entry(&path, dir, ';') || !asker.ask(&format!("Take {} off your PATH?", dir.display())) {
        return Ok(());
    }
    let dir_text = dir.to_string_lossy();
    let dir_text = dir_text.trim_end_matches(['\\', '/']);
    let kept: Vec<&str> = path
        .split(';')
        .filter(|e| !e.trim_end_matches(['\\', '/']).eq_ignore_ascii_case(dir_text))
        .collect();
    set_user_path(&kept.join(";"))?;
    println!("  taken off");
    Ok(())
}

// ---------------------------------------------------------------- Midnight Commander

/// The user's extension file of mc: `$XDG_CONFIG_HOME/mc/mc.ext.ini`, or `~/.config/mc/mc.ext.ini`.
fn mc_ext_path() -> Option<PathBuf> {
    env_dir("XDG_CONFIG_HOME")
        .or_else(|| env_dir("HOME").map(|h| h.join(".config")))
        .map(|c| c.join("mc").join("mc.ext.ini"))
}

/// The system's extension file of mc, which mc reads when the user has none (`/etc/mc` first, as mc does).
fn mc_system_file() -> Option<PathBuf> {
    [
        "/etc/mc/mc.ext.ini",
        "/usr/share/mc/mc.ext.ini",
        "/usr/local/etc/mc/mc.ext.ini",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}

/// Whether mc seems to be installed: its settings folder or its system file.
fn mc_is_there() -> bool {
    !cfg!(windows)
        && (mc_system_file().is_some()
            || mc_ext_path()
                .and_then(|p| p.parent().map(Path::is_dir))
                .unwrap_or(false))
}

/// How mc's shell is to call the program: its path, quoted for the shell when it has to be.
fn command_of(exe: &Path) -> String {
    let text = exe.to_string_lossy();
    if text.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c)) {
        text.into_owned()
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

fn is_header(line: &str) -> bool {
    line.trim_start().starts_with('[')
}

/// `text` without its sections `[Peekel]`: each up to the first empty line (taken too) or the next section, so that the comments
/// over the next one stay (ours is written with no empty line in it, and one after it).
pub fn without_mc_section(text: &str) -> String {
    let mut out = String::new();
    let mut ours = false;
    for line in text.split_inclusive('\n') {
        if is_header(line) {
            ours = line.trim() == "[Peekel]";
        } else if ours && line.trim().is_empty() {
            ours = false;
            continue;
        }
        if !ours {
            out.push_str(line);
        }
    }
    out
}

/// `text` (an `mc.ext.ini`) with `section` as its first section, right after `[mc.ext.ini]` and its keys; a section `[Peekel]` that
/// was there is taken out. A text with no `[mc.ext.ini]` gets one (mc does not read a file without it).
pub fn with_mc_section(text: &str, section: &str) -> String {
    let text = without_mc_section(text);
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let Some(head) = lines.iter().position(|l| l.trim() == "[mc.ext.ini]") else {
        return format!("[mc.ext.ini]\nVersion=4.0\n\n{section}\n{text}");
    };
    let mut at = head + 1;
    while at < lines.len() {
        let line = lines[at].trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') || is_header(line) {
            break;
        }
        at += 1;
    }
    let mut out: String = lines[..at].concat();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    out.push_str(section);
    let rest = lines[at..].concat();
    if !rest.starts_with('\n') && !rest.starts_with("\r\n") && !rest.is_empty() {
        out.push('\n');
    }
    out.push_str(&rest);
    out
}

fn setup_mc(command: &str, asker: &Asker) -> Result<(), String> {
    if cfg!(windows) {
        return Err("Midnight Commander is set up on Linux (or macOS), not on Windows".to_string());
    }
    if !mc_is_there() {
        return Err(
            "Midnight Commander was not found (no /etc/mc/mc.ext.ini, no ~/.config/mc): nothing is changed".to_string(),
        );
    }
    let path = mc_ext_path().ok_or("HOME is not set")?;
    let (text, from) = if path.is_file() {
        (
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?,
            None,
        )
    } else if let Some(system) = mc_system_file() {
        (
            std::fs::read_to_string(&system).map_err(|e| format!("{}: {e}", system.display()))?,
            Some(system),
        )
    } else {
        (String::new(), None)
    };
    let new = with_mc_section(&text, &files::mc_section_for(command));
    if new == text {
        println!("Midnight Commander is set up already ({}).", path.display());
        return Ok(());
    }
    let what = match &from {
        Some(system) => format!(
            "Make {} (a copy of {}, which mc stops reading once you have your own) with the section [Peekel] first?",
            path.display(),
            system.display()
        ),
        None if path.is_file() => format!("Put the section [Peekel] first in {}?", path.display()),
        None => format!("Make {} with the section [Peekel]?", path.display()),
    };
    if !asker.ask(&what) {
        return Ok(());
    }
    if path.is_file() {
        let backup = backup_name(&path);
        std::fs::copy(&path, &backup).map_err(|e| format!("{}: {e}", backup.display()))?;
        println!("  the file as it was: {}", backup.display());
    } else if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(&path, new).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("  done; mc reads the file when it starts: restart it if it is open");
    Ok(())
}

// ---------------------------------------------------------------- Far Manager

/// Far.exe: the one given, else one on PATH, else in the usual folders.
fn find_far(given: Option<&Path>) -> Option<PathBuf> {
    if let Some(given) = given {
        let far = if given.is_dir() {
            given.join("Far.exe")
        } else {
            given.to_path_buf()
        };
        return far.is_file().then_some(far);
    }
    let on_path = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).map(|d| d.join("Far.exe")).collect::<Vec<_>>())
        .unwrap_or_default();
    let usual = [
        ("LOCALAPPDATA", "Far Manager x64"),
        ("LOCALAPPDATA", "Far Manager"),
        ("ProgramFiles", "Far Manager"),
        ("ProgramFiles(x86)", "Far Manager"),
    ]
    .into_iter()
    .filter_map(|(var, folder)| env_dir(var).map(|d| d.join(folder).join("Far.exe")));
    on_path.into_iter().chain(usual).find(|p| p.is_file())
}

fn far_running() -> bool {
    hidden(Command::new("tasklist").args(["/FI", "IMAGENAME eq far.exe", "/NH", "/FO", "CSV"]))
        .output()
        .is_ok_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .to_lowercase()
                .contains("\"far.exe\"")
        })
}

/// A folder named by Far's ini, with `%FARHOME%` and the variables of the environment put in.
fn expand(text: &str, home: &Path) -> PathBuf {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('%') else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let name = &after[..end];
        if name.eq_ignore_ascii_case("FARHOME") {
            out.push_str(&home.to_string_lossy());
        } else {
            out.push_str(&std::env::var(name).unwrap_or_default());
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    PathBuf::from(out)
}

/// The folder of Far's roaming profile, where `associations.db` is: as `Far.exe.ini` says (`UseSystemProfiles`, `UserProfileDir`), else
/// `%APPDATA%\Far Manager\Profile`. `PEEKEL_FAR_PROFILE` names another (for tests: Far is then called with it).
fn far_profile(far: &Path) -> Option<PathBuf> {
    if let Some(given) = env_dir("PEEKEL_FAR_PROFILE") {
        return Some(given);
    }
    let home = far.parent()?.to_path_buf();
    let ini = std::fs::read_to_string(home.join("Far.exe.ini")).unwrap_or_default();
    let value = |key: &str| {
        ini.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim().eq_ignore_ascii_case(key) && !l.trim_start().starts_with(';')).then(|| v.trim().to_string())
        })
    };
    if value("UseSystemProfiles").as_deref() == Some("0") {
        return Some(expand(
            &value("UserProfileDir").unwrap_or_else(|| "%FARHOME%\\Profile".to_string()),
            &home,
        ));
    }
    env_dir("APPDATA").map(|d| d.join("Far Manager").join("Profile"))
}

/// Runs Far with `-export` or `-import` of `file` (and the test profile, when there is one); Far does it with no window and closes.
fn far_call(far: &Path, verb: &str, file: &Path) -> Result<(), String> {
    let mut command = Command::new(far);
    command.arg(verb).arg(file);
    if let Some(profile) = env_dir("PEEKEL_FAR_PROFILE") {
        command.arg(&profile).arg(&profile);
    }
    let mut child = hidden(&mut command)
        .spawn()
        .map_err(|e| format!("{}: {e}", far.display()))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) if started.elapsed() > Duration::from_secs(60) => {
                let _ = child.kill();
                return Err(format!("Far did not finish {verb} in a minute"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(format!("{}: {e}", far.display())),
        }
    }
}

fn temp_file(what: &str) -> PathBuf {
    std::env::temp_dir().join(format!("peekel-{what}-{}-{}.farconfig", std::process::id(), stamp()))
}

/// Far's associations now, as the text of each `filetype` element.
fn far_export(far: &Path) -> Result<Vec<String>, String> {
    let file = temp_file("export");
    let called = far_call(far, "-export", &file);
    let text = std::fs::read_to_string(&file);
    let _ = std::fs::remove_file(&file);
    called?;
    let text = text.map_err(|e| format!("Far wrote no settings: {e}"))?;
    filetypes(&text)
}

/// The `filetype` elements of the `associations` of a `.farconfig`, each as its text in the file.
pub fn filetypes(config: &str) -> Result<Vec<String>, String> {
    let document = roxmltree_parse(config)?;
    let Some(associations) = document.descendants().find(|n| n.has_tag_name("associations")) else {
        return Ok(Vec::new());
    };
    Ok(associations
        .children()
        .filter(|n| n.has_tag_name("filetype"))
        .map(|n| config[n.range()].to_string())
        .collect())
}

fn roxmltree_parse(text: &str) -> Result<peekel_core::xml::Document<'_>, String> {
    peekel_core::xml::parse(text).map_err(|e| format!("Far's settings could not be read: {e}"))
}

/// The mask, the description and the commands (type, enabled, command line) of a `filetype`.
type Meaning = (String, String, Vec<(String, String, String)>);

/// What a `filetype` says, for comparing (the text may differ in spaces and quoting): its mask, description and commands.
fn meaning(filetype: &str) -> Option<Meaning> {
    let document = peekel_core::xml::parse(filetype).ok()?;
    let node = document.root_element();
    let attr = |n: peekel_core::xml::Node<'_, '_>, name: &str| n.attribute(name).unwrap_or("").to_string();
    let commands = node
        .children()
        // an association made in Far's dialog has all six commands, those left empty with no text
        .filter(|c| c.has_tag_name("command") && !c.attribute("command").unwrap_or("").trim().is_empty())
        .map(|c| (attr(c, "type"), attr(c, "enabled"), attr(c, "command")))
        .collect();
    Some((attr(node, "mask"), attr(node, "description"), commands))
}

/// Whether a `filetype` is one of ours: every command it has runs `peekel.exe`.
pub fn is_ours(filetype: &str) -> bool {
    meaning(filetype).is_some_and(|(_, _, commands)| {
        !commands.is_empty()
            && commands
                .iter()
                .all(|(_, _, command)| command.to_lowercase().contains("peekel.exe"))
    })
}

fn escape_attribute(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Our association: the mask of every file the viewer reads, F3 runs `exe`.
pub fn far_filetype(exe: &Path) -> String {
    let command = format!("\"{}\" \"!.!\"", exe.display());
    format!(
        "<filetype mask=\"{}\" description=\"Peekel\">\n<command type=\"2\" enabled=\"1\" command=\"{}\"/>\n</filetype>",
        escape_attribute(&files::far_mask()),
        escape_attribute(&command)
    )
}

fn far_config(filetypes: &[String]) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<farconfig version=\"3.0\">\n<associations>\n{}\n</associations>\n</farconfig>\n",
        filetypes.join("\n")
    )
}

/// A short name of a `filetype` for the user: its description, or the start of its mask.
fn label(filetype: &str) -> String {
    meaning(filetype)
        .map(|(mask, description, _)| {
            if description.is_empty() {
                mask.chars().take(60).collect()
            } else {
                description
            }
        })
        .unwrap_or_default()
}

/// Makes Far's associations `wanted`: moves `associations.db` aside as the backup, imports `wanted` into an empty one, reads it back;
/// on a difference the backup is put back.
fn far_replace(far: &Path, wanted: &[String]) -> Result<PathBuf, String> {
    let profile = far_profile(far).ok_or("Far's profile folder is not known")?;
    let db = profile.join("associations.db");
    let backup = backup_name(&db);
    let had = db.is_file();
    if had {
        std::fs::rename(&db, &backup).map_err(|e| format!("{}: {e}", db.display()))?;
    }
    let put_back = |why: String| -> String {
        if had {
            let _ = std::fs::remove_file(&db);
            if std::fs::rename(&backup, &db).is_err() {
                return format!(
                    "{why}; and the backup could not be put back: copy {} to {}",
                    backup.display(),
                    db.display()
                );
            }
        }
        why
    };
    // with the database aside Far must show no association: else it reads another profile than this one
    match far_export(far) {
        Ok(left) if left.is_empty() => {}
        Ok(_) => {
            return Err(put_back(format!(
                "Far's settings are not in {} (Far.exe.ini?)",
                profile.display()
            )));
        }
        Err(e) => return Err(put_back(e)),
    }
    let file = temp_file("import");
    std::fs::write(&file, far_config(wanted)).map_err(|e| put_back(format!("{}: {e}", file.display())))?;
    let imported = far_call(far, "-import", &file);
    let _ = std::fs::remove_file(&file);
    imported.map_err(put_back)?;
    let now = far_export(far).map_err(put_back)?;
    let same = now.len() == wanted.len() && now.iter().zip(wanted).all(|(a, b)| meaning(a) == meaning(b));
    if !same {
        return Err(put_back("Far did not take the list as it was given".to_string()));
    }
    Ok(backup)
}

fn setup_far(far: &Path, exe: &Path, asker: &Asker) -> Result<(), String> {
    if far_running() {
        return Err("Far Manager is running: close it and run `peekel --setup far`".to_string());
    }
    let now = far_export(far)?;
    let ours = far_filetype(exe);
    if now.first().is_some_and(|f| meaning(f) == meaning(&ours)) && now[1..].iter().all(|f| !is_ours(f)) {
        println!("Far Manager is set up already.");
        return Ok(());
    }
    let replaced: Vec<String> = now.iter().filter(|f| is_ours(f)).map(|f| label(f)).collect();
    let mut question = "Make Far's F3 open these files in peekel".to_string();
    if !replaced.is_empty() {
        question.push_str(&format!(
            " (in place of your association \"{}\")",
            replaced.join("\", \"")
        ));
    }
    question.push('?');
    if !asker.ask(&question) {
        return Ok(());
    }
    let mut wanted = vec![ours];
    wanted.extend(now.into_iter().filter(|f| !is_ours(f)));
    let backup = far_replace(far, &wanted)?;
    println!("  done; Far's associations as they were: {}", backup.display());
    Ok(())
}

fn unset_far(far: &Path, asker: &Asker) -> Result<(), String> {
    let now = far_export(far)?;
    let ours: Vec<String> = now.iter().filter(|f| is_ours(f)).map(|f| label(f)).collect();
    if ours.is_empty() {
        return Ok(());
    }
    if far_running() {
        return Err("Far Manager is running: close it to take its association away".to_string());
    }
    if !asker.ask(&format!("Take Far's association \"{}\" away?", ours.join("\", \""))) {
        return Ok(());
    }
    let wanted: Vec<String> = now.into_iter().filter(|f| !is_ours(f)).collect();
    let backup = far_replace(far, &wanted)?;
    println!("  done; Far's associations as they were: {}", backup.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYSTEM: &str =
        "# a comment\n[mc.ext.ini]\nVersion=4.0\n\n# archives\n[tar]\nRegex=\\\\.tar$\nOpen=x\n\n[Default]\nOpen=y\n";

    #[test]
    fn the_section_of_mc_goes_first_and_replaces_the_one_there() {
        let section = files::mc_section_for("peekel");
        let once = with_mc_section(SYSTEM, &section);
        assert!(
            once.starts_with("# a comment\n[mc.ext.ini]\nVersion=4.0\n\n[Peekel]\n"),
            "{once}"
        );
        assert!(
            once.contains("Open=peekel %f\nView=peekel %f\n\n# archives\n[tar]"),
            "{once}"
        );
        assert_eq!(with_mc_section(&once, &section), once, "the same twice is the same");
        let other = files::mc_section_for("/opt/peekel");
        let twice = with_mc_section(&once, &other);
        assert_eq!(twice.matches("[Peekel]").count(), 1, "{twice}");
        assert!(twice.contains("View=/opt/peekel %f"), "{twice}");
        assert_eq!(without_mc_section(&twice), without_mc_section(SYSTEM));
        assert_eq!(without_mc_section(&once), SYSTEM, "taken out, the file is as it was");
    }

    #[test]
    fn a_file_of_mc_without_its_head_gets_one() {
        let made = with_mc_section("", &files::mc_section_for("peekel"));
        assert!(made.starts_with("[mc.ext.ini]\nVersion=4.0\n\n[Peekel]\n"), "{made}");
        let only = with_mc_section("[mc.ext.ini]\nVersion=4.0", &files::mc_section_for("peekel"));
        assert!(only.starts_with("[mc.ext.ini]\nVersion=4.0\n\n[Peekel]\n"), "{only}");
    }

    #[test]
    fn a_path_with_a_space_is_quoted_for_the_shell_of_mc() {
        assert_eq!(
            command_of(Path::new("/home/a/.local/bin/peekel")),
            "/home/a/.local/bin/peekel"
        );
        assert_eq!(command_of(Path::new("/home/a b/it's")), "'/home/a b/it'\\''s'");
    }

    #[test]
    fn far_associations_of_ours_are_told_from_the_others() {
        let config = format!(
            "<farconfig><associations>\n<filetype mask=\"*.txt\" description=\"Mine\"><command type=\"4\" enabled=\"1\" command=\"notepad !.!\"/></filetype>\n\
             <filetype mask=\"*.png\" description=\"Pictures (SixView)\"><command type=\"0\" enabled=\"1\" command=\"\"/><command type=\"2\" enabled=\"1\" command=\"&quot;C:\\x\\peekel.exe&quot; &quot;!.!&quot;\"/><command type=\"4\" enabled=\"1\" command=\"\"/></filetype>\n\
             <filetype mask=\"*.pdf\" description=\"Both\"><command type=\"2\" enabled=\"1\" command=\"C:\\x\\PEEKEL.EXE !.!\"/><command type=\"4\" enabled=\"1\" command=\"be !.!\"/></filetype>\n\
             {}\n</associations></farconfig>",
            far_filetype(Path::new("C:\\Users\\a & b\\peekel.exe"))
        );
        let all = filetypes(&config).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(all.len(), 4);
        let ours: Vec<bool> = all.iter().map(|f| is_ours(f)).collect();
        assert_eq!(ours, vec![false, true, false, true]);
        assert_eq!(label(&all[1]), "Pictures (SixView)");
        let mine = meaning(&all[3]).unwrap_or_default();
        assert_eq!(mine.0, files::far_mask());
        assert_eq!(
            mine.2,
            vec![(
                "2".into(),
                "1".into(),
                "\"C:\\Users\\a & b\\peekel.exe\" \"!.!\"".into()
            )]
        );
        let again = filetypes(&far_config(&all)).unwrap_or_default();
        assert_eq!(
            again.iter().map(|f| meaning(f)).collect::<Vec<_>>(),
            all.iter().map(|f| meaning(f)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_folder_of_far_ini_is_expanded() {
        let home = Path::new("C:\\Far");
        assert_eq!(expand("%FARHOME%\\Profile", home), PathBuf::from("C:\\Far\\Profile"));
        assert_eq!(expand("D:\\x%", home), PathBuf::from("D:\\x%"));
    }

    #[test]
    fn a_folder_is_on_a_path_whatever_its_last_slash() {
        assert!(has_entry(
            "C:\\a;C:\\Users\\x\\Programs\\peekel\\;D:\\",
            Path::new("c:\\users\\x\\programs\\peekel"),
            ';'
        ));
        assert!(!has_entry("C:\\a;C:\\peekel2", Path::new("C:\\peekel"), ';'));
        assert!(has_entry(
            "/usr/bin:/home/a/.local/bin",
            Path::new("/home/a/.local/bin/"),
            ':'
        ));
    }
}
