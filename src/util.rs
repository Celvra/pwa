use crate::config::{Config, LocalRepos};
use crate::repo;

use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{stderr, stdin, stdout, BufRead, Read, Write};
use std::mem::take;
use std::ops::Range;
use std::os::fd::{AsFd, OwnedFd};

use alpm::{Package, PackageReason};
use alpm_utils::depends::{satisfies_dep, satisfies_provide};
use alpm_utils::{AsTarg, DbListExt, Targ};
use anyhow::Result;
use nix::sys::termios::{tcgetattr, tcsetattr, LocalFlags, SetArg};
use nix::unistd::{dup2_stdin, dup2_stdout};
use tr::tr;
use unicode_width::UnicodeWidthStr;

#[derive(Debug)]
pub struct NumberMenu<'a> {
    pub in_range: Vec<Range<usize>>,
    pub ex_range: Vec<Range<usize>>,
    pub in_word: Vec<&'a str>,
    pub ex_word: Vec<&'a str>,
}

pub fn pkg_base_or_name(pkg: &Package) -> &str {
    pkg.base().unwrap_or_else(|| pkg.name())
}

pub fn split_repo_aur_targets<'a, T: AsTarg>(
    config: &mut Config,
    targets: &'a [T],
) -> Result<(Vec<Targ<'a>>, Vec<Targ<'a>>)> {
    let mut local = Vec::new();
    let mut aur = Vec::new();

    let cb = config.alpm.take_raw_question_cb();
    let empty: [&str; 0] = [];
    config.alpm.set_ignorepkgs(empty.iter())?;
    config.alpm.set_ignoregroups(empty.iter())?;

    let dbs = config.alpm.syncdbs();

    for targ in targets {
        let targ = targ.as_targ();
        if !config.mode.repo() {
            aur.push(targ);
        } else if !config.mode.aur() && !config.mode.pkgbuild() {
            local.push(targ);
        } else if let Some(repo) = targ.repo {
            if config.alpm.syncdbs().iter().any(|db| db.name() == repo) {
                local.push(targ);
            } else if config.pkgbuild_repos.repo(repo).is_some()
                || repo == config.aur_namespace()
                || repo == "."
            {
                aur.push(targ);
            } else {
                local.push(targ);
            }
        } else if dbs.pkg(targ.pkg).is_ok()
            || dbs.find_target_satisfier(targ.pkg).is_some()
            || dbs
                .iter()
                .filter(|db| targ.repo.is_none() || db.name() == targ.repo.unwrap())
                .any(|db| db.group(targ.pkg).is_ok())
        {
            local.push(targ);
        } else {
            aur.push(targ);
        }
    }

    config.alpm.set_raw_question_cb(cb);
    config
        .alpm
        .set_ignorepkgs(config.pacman.ignore_pkg.iter())?;
    config
        .alpm
        .set_ignorepkgs(config.pacman.ignore_pkg.iter())?;

    Ok((local, aur))
}

pub fn split_repo_aur_info<'a, T: AsTarg>(
    config: &Config,
    targets: &'a [T],
) -> Result<(Vec<Targ<'a>>, Vec<Targ<'a>>)> {
    let mut local = Vec::new();
    let mut aur = Vec::new();

    let dbs = config.alpm.syncdbs();

    for targ in targets {
        let targ = targ.as_targ();
        if !config.mode.repo() {
            aur.push(targ);
        } else if !config.mode.aur() && !config.mode.pkgbuild() {
            local.push(targ);
        } else if let Some(repo) = targ.repo {
            if config.alpm.syncdbs().iter().any(|db| db.name() == repo) {
                local.push(targ);
            } else {
                aur.push(targ);
            }
        } else if dbs.pkg(targ.pkg).is_ok() {
            local.push(targ);
        } else {
            aur.push(targ);
        }
    }

    Ok((local, aur))
}

pub fn ask(config: &Config, question: &str, default: bool) -> bool {
    let action = config.color.action;
    let bold = config.color.bold;
    let yn = if default {
        tr!("[Y/n]:")
    } else {
        tr!("[y/N]:")
    };
    print!(
        "{} {} {} ",
        action.paint("::"),
        bold.paint(question),
        bold.paint(yn)
    );
    let _ = stdout().lock().flush();
    if config.no_confirm {
        println!();
        return default;
    }
    let stdin = stdin();
    let mut input = String::new();
    let _ = stdin.read_line(&mut input);
    let input = input.to_lowercase();
    let input = input.trim();

    if input == tr!("y") || input == tr!("yes") {
        true
    } else if input.trim().is_empty() {
        default
    } else {
        false
    }
}

/// The answer to a three way prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YesNoEdit {
    Yes,
    No,
    /// The user wants to keep talking to the ai.
    Edit,
}

/// Asks a question that can also be answered by continuing the conversation.
///
/// `yes` controls whether y is offered at all: when the ai found nothing there
/// is nothing to accept, so the prompt is only n or e.
pub fn ask_yne(config: &Config, question: &str, yes: bool) -> YesNoEdit {
    let action = config.color.action;
    let bold = config.color.bold;

    let options = if yes { "[Y/n/e]" } else { "[N/e]" };
    print!(
        "{} {} {} ",
        action.paint("::"),
        bold.paint(question),
        bold.paint(options)
    );
    let _ = stdout().lock().flush();

    if config.no_confirm {
        println!();
        return if yes { YesNoEdit::Yes } else { YesNoEdit::No };
    }

    let mut input = String::new();
    let _ = stdin().read_line(&mut input);
    let input = input.trim().to_lowercase();

    if input == "e" || input == tr!("e") {
        YesNoEdit::Edit
    } else if input.is_empty() {
        if yes {
            YesNoEdit::Yes
        } else {
            YesNoEdit::No
        }
    } else if input == tr!("y") || input == tr!("yes") {
        YesNoEdit::Yes
    } else {
        YesNoEdit::No
    }
}

pub fn input(config: &Config, question: &str) -> String {
    let action = config.color.action;
    let bold = config.color.bold;
    println!("{} {}", action.paint("::"), bold.paint(question));
    print!("{} ", action.paint("::"));
    let _ = stdout().lock().flush();
    if config.no_confirm {
        println!();
        return "".into();
    }

    let mut stdin_handle = std::io::stdin();
    let original_termios = match tcgetattr(&stdin_handle) {
        Ok(t) => t,
        Err(_) => {
            let mut input = String::new();
            let _ = stdin_handle.read_line(&mut input);
            return input;
        }
    };

    let mut raw = original_termios.clone();
    raw.local_flags
        .remove(LocalFlags::ICANON | LocalFlags::ECHO);
    let _ = tcsetattr(&stdin_handle, SetArg::TCSANOW, &raw);

    let prompt = format!("{} ", action.paint("::"));

    // The buffer is a list of characters, not bytes. Indexing a String by byte
    // offset panics the moment a multi byte character is typed, so the cursor
    // counts characters and only ever converts when drawing.
    let mut buffer: Vec<char> = Vec::new();
    let mut cursor = 0usize;

    let restore = |handle: &std::io::Stdin| {
        let _ = tcsetattr(handle, SetArg::TCSANOW, &original_termios);
    };

    // Redraws the line and leaves the cursor where the caller thinks it is.
    // \x1b[K clears whatever the previous, longer line left behind.
    let redraw = |buffer: &[char], cursor: usize| {
        let text: String = buffer.iter().collect();
        print!("\r{}{}\x1b[K", prompt, text);

        let tail: String = buffer[cursor..].iter().collect();
        let back = UnicodeWidthStr::width(tail.as_str());
        if back > 0 {
            print!("\x1b[{}D", back);
        }
        let _ = stdout().lock().flush();
    };

    loop {
        let mut byte = [0u8; 1];
        if stdin_handle.read_exact(&mut byte).is_err() {
            restore(&stdin_handle);
            return buffer.into_iter().collect();
        }

        match byte[0] {
            b'\n' | b'\r' => {
                print!("\r\n");
                let _ = stdout().lock().flush();
                restore(&stdin_handle);
                return buffer.into_iter().collect();
            }
            // Backspace only ever removes a character the user typed, so the
            // prompt can not be eaten.
            0x7F | 0x08 => {
                if cursor > 0 {
                    cursor -= 1;
                    buffer.remove(cursor);
                    redraw(&buffer, cursor);
                }
            }
            // Ctrl-D on an empty line ends the prompt, like a shell.
            0x04 => {
                if buffer.is_empty() {
                    print!("\r\n");
                    let _ = stdout().lock().flush();
                    restore(&stdin_handle);
                    return String::new();
                }
            }
            // Ctrl-U clears the line.
            0x15 => {
                buffer.clear();
                cursor = 0;
                redraw(&buffer, cursor);
            }
            0x1B => {
                let mut seq = [0u8; 2];
                if stdin_handle.read_exact(&mut seq).is_ok() && seq[0] == b'[' {
                    match seq[1] {
                        b'D' if cursor > 0 => {
                            cursor -= 1;
                            redraw(&buffer, cursor);
                        }
                        b'C' if cursor < buffer.len() => {
                            cursor += 1;
                            redraw(&buffer, cursor);
                        }
                        // Home and End arrive as \x1b[H and \x1b[F on most terminals.
                        b'H' => {
                            cursor = 0;
                            redraw(&buffer, cursor);
                        }
                        b'F' => {
                            cursor = buffer.len();
                            redraw(&buffer, cursor);
                        }
                        _ => {}
                    }
                }
            }
            // Remaining control characters are not editing keys.
            b if b < 0x20 => {}
            first => {
                // A UTF-8 sequence is one leading byte plus its continuations.
                let extra = if first < 0x80 {
                    0
                } else if first & 0xE0 == 0xC0 {
                    1
                } else if first & 0xF0 == 0xE0 {
                    2
                } else if first & 0xF8 == 0xF0 {
                    3
                } else {
                    // A stray continuation byte; there is nothing to insert.
                    continue;
                };

                let mut bytes = [0u8; 4];
                bytes[0] = first;
                if extra > 0 && stdin_handle.read_exact(&mut bytes[1..=extra]).is_err() {
                    continue;
                }

                let Ok(text) = std::str::from_utf8(&bytes[..=extra]) else {
                    continue;
                };

                for ch in text.chars() {
                    buffer.insert(cursor, ch);
                    cursor += 1;
                }
                redraw(&buffer, cursor);
            }
        }
    }
}

pub fn unneeded_pkgs(config: &Config, keep_optional: bool) -> Vec<&str> {
    let db = config.alpm.localdb();
    let mut next = db
        .pkgs()
        .into_iter()
        .filter(|p| p.reason() == PackageReason::Explicit)
        .collect::<Vec<_>>();
    let mut deps = db
        .pkgs()
        .into_iter()
        .filter(|p| p.reason() != PackageReason::Explicit)
        .map(|p| (p.name(), p))
        .collect::<BTreeMap<_, _>>();

    let mut provides: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for dep in deps.values() {
        for prov in dep.provides() {
            provides.entry(prov.name()).or_default().push((*dep, prov));
        }
    }

    while !next.is_empty() {
        for new in take(&mut next) {
            let opt = keep_optional.then(|| new.optdepends());
            let depends = new.depends().into_iter().chain(opt.into_iter().flatten());

            for dep in depends {
                if let Entry::Occupied(entry) = deps.entry(dep.name()) {
                    let pkg = entry.get();
                    if satisfies_dep(dep, pkg.name(), pkg.version()) {
                        next.push(entry.remove());
                    }
                }
                if let Entry::Occupied(mut entry) = provides.entry(dep.name()) {
                    let provides = entry
                        .get_mut()
                        .extract_if(.., |(_, prov)| satisfies_provide(dep, prov))
                        .filter_map(|(pkg, _)| deps.remove(pkg.name()));
                    next.extend(provides);
                };
            }
        }
    }

    deps.into_keys().collect::<Vec<_>>()
}

/// Whether an input should be handled by [`NumberMenu`] rather than treated as
/// natural language.
///
/// Deliberately conservative: anything the number menu understands today must
/// still be answered yes here so enabling the ai layer never changes how an
/// existing selection is parsed. A word only counts as natural language when it
/// is neither a number range nor the name of something in the list.
pub fn is_number_menu(input: &str, words: &[&str]) -> bool {
    let input = input.trim();

    if input.is_empty() {
        return true;
    }

    let mut any = false;

    for word in input
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|s| !s.is_empty())
    {
        any = true;
        let word = word.trim_start_matches('^');

        if word.is_empty() {
            continue;
        }

        // 5 or 2-7
        let numeric = match word.split_once('-') {
            Some((start, end)) => {
                !start.is_empty()
                    && !end.is_empty()
                    && start.chars().all(|c| c.is_ascii_digit())
                    && end.chars().all(|c| c.is_ascii_digit())
            }
            None => word.chars().all(|c| c.is_ascii_digit()),
        };

        if numeric {
            continue;
        }

        // A repo or package name that was printed in the menu.
        if words.contains(&word) {
            continue;
        }

        return false;
    }

    any
}

impl<'a> NumberMenu<'a> {
    pub fn new(input: &'a str) -> Self {
        let mut include_range = Vec::new();
        let mut exclude_range = Vec::new();
        let mut include_repo = Vec::new();
        let mut exclude_repo = Vec::new();

        let words = input
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|s| !s.is_empty());

        for mut word in words {
            let mut invert = false;
            if word.starts_with('^') {
                word = word.trim_start_matches('^');
                invert = true;
            }

            let mut split = word.split('-');
            let start_str = split.next().unwrap();

            let start = match start_str.parse::<usize>() {
                Ok(start) => start,
                Err(_) => {
                    if invert {
                        exclude_repo.push(start_str);
                    } else {
                        include_repo.push(start_str);
                    }
                    continue;
                }
            };

            let end = match split.next() {
                Some(end) => end,
                None => {
                    if invert {
                        exclude_range.push(start..start + 1);
                    } else {
                        include_range.push(start..start + 1);
                    }
                    continue;
                }
            };

            match end.parse::<usize>() {
                Ok(end) => {
                    if invert {
                        exclude_range.push(start..end + 1)
                    } else {
                        include_range.push(start..end + 1)
                    }
                }
                _ => {
                    if invert {
                        exclude_repo.push(start_str)
                    } else {
                        include_repo.push(start_str)
                    }
                }
            }
        }

        NumberMenu {
            in_range: include_range,
            ex_range: exclude_range,
            in_word: include_repo,
            ex_word: exclude_repo,
        }
    }

    pub fn contains(&self, n: usize, word: &str) -> bool {
        if self.in_range.iter().any(|r| r.contains(&n)) || self.in_word.contains(&word) {
            true
        } else if self.ex_range.iter().any(|r| r.contains(&n)) || self.ex_word.contains(&word) {
            false
        } else {
            self.in_range.is_empty() && self.in_word.is_empty()
        }
    }
}

pub fn get_provider(max: usize, no_confirm: bool) -> usize {
    let mut input = String::new();

    loop {
        print!("\n{}", tr!("Enter a number (default=1): "));
        let _ = stdout().lock().flush();
        input.clear();

        if !no_confirm {
            let stdin = stdin();
            let mut stdin = stdin.lock();
            let _ = stdin.read_line(&mut input);
        }

        let num = input.trim();
        if num.is_empty() {
            return 0;
        }

        let num = match num.parse::<usize>() {
            Err(_) => {
                eprintln!("{}", tr!("invalid number: {}", num));
                continue;
            }
            Ok(num) => num,
        };

        if num < 1 || num > max {
            eprintln!(
                "{}",
                tr!(
                    "invalid value: {n} is not between 1 and {max}",
                    n = num,
                    max = max
                )
            );
            continue;
        }

        return num - 1;
    }
}

pub fn split_repo_aur_pkgs<S: AsRef<str> + Clone>(config: &Config, pkgs: &[S]) -> (Vec<S>, Vec<S>) {
    let mut aur = Vec::new();
    let mut repo = Vec::new();
    let (repo_dbs, aur_dbs) = repo::repo_aur_dbs(config);

    for pkg in pkgs {
        if repo_dbs.pkg(pkg.as_ref()).is_ok() {
            repo.push(pkg.clone());
        } else if config.repos == LocalRepos::None || aur_dbs.pkg(pkg.as_ref()).is_ok() {
            aur.push(pkg.clone());
        }
    }

    (repo, aur)
}

pub fn repo_aur_pkgs(config: &Config) -> (Vec<&alpm::Package>, Vec<&alpm::Package>) {
    if config.repos != LocalRepos::None {
        let (repo, aur) = repo::repo_aur_dbs(config);
        let repo = repo.iter().flat_map(|db| db.pkgs()).collect::<Vec<_>>();
        let aur = aur.iter().flat_map(|db| db.pkgs()).collect::<Vec<_>>();
        (repo, aur)
    } else {
        let (repo, aur) = config
            .alpm
            .localdb()
            .pkgs()
            .iter()
            .partition(|pkg| config.alpm.syncdbs().pkg(pkg.name()).is_ok());
        (repo, aur)
    }
}

pub fn redirect_to_stderr() -> Result<OwnedFd> {
    let stdout = stdout().as_fd().try_clone_to_owned()?;
    dup2_stdout(stderr())?;
    Ok(stdout)
}

pub fn reopen_stdin() -> Result<()> {
    let file = File::open("/dev/tty")?;
    dup2_stdin(&file)?;
    Ok(())
}

pub fn reopen_stdout<Fd: AsFd>(file: Fd) -> Result<()> {
    dup2_stdout(file)?;
    Ok(())
}

pub fn is_arch_repo(name: &str) -> bool {
    matches!(
        name,
        "testing"
            | "community-testing"
            | "core"
            | "extra"
            | "community"
            | "core-testing"
            | "extra-testing"
            | "multilib-testing"
    )
}

#[cfg(test)]
mod tests {
    use super::{is_number_menu, YesNoEdit};

    const WORDS: &[&str] = &["core", "extra", "aur", "chromium"];

    /// The answer parsing used by `ask_yne`, kept in step with it so the three
    /// way prompt can be tested without a terminal.
    fn parse_yne(input: &str, yes: bool) -> YesNoEdit {
        let input = input.trim().to_lowercase();

        if input == "e" {
            YesNoEdit::Edit
        } else if input.is_empty() {
            if yes {
                YesNoEdit::Yes
            } else {
                YesNoEdit::No
            }
        } else if input == "y" || input == "yes" {
            YesNoEdit::Yes
        } else {
            YesNoEdit::No
        }
    }

    #[test]
    fn e_always_continues_the_conversation() {
        assert_eq!(parse_yne("e", true), YesNoEdit::Edit);
        assert_eq!(parse_yne("e", false), YesNoEdit::Edit);
        assert_eq!(parse_yne(" E \n", true), YesNoEdit::Edit);
    }

    #[test]
    fn enter_takes_the_offered_default() {
        // With a selection on offer, enter accepts it.
        assert_eq!(parse_yne("", true), YesNoEdit::Yes);
        // With nothing to install there is no yes to default to.
        assert_eq!(parse_yne("", false), YesNoEdit::No);
    }

    #[test]
    fn anything_else_declines() {
        assert_eq!(parse_yne("n", true), YesNoEdit::No);
        assert_eq!(parse_yne("no", true), YesNoEdit::No);
        assert_eq!(parse_yne("q", true), YesNoEdit::No);
        assert_eq!(parse_yne("y", true), YesNoEdit::Yes);
    }

    #[test]
    fn number_menu_inputs_are_not_natural_language() {
        // Everything NumberMenu handles today must keep being parsed as numbers.
        assert!(is_number_menu("1", WORDS));
        assert!(is_number_menu("1 2 3", WORDS));
        assert!(is_number_menu("1,2,3", WORDS));
        assert!(is_number_menu("1-3", WORDS));
        assert!(is_number_menu("1-3 5 7-9", WORDS));
        assert!(is_number_menu("^2", WORDS));
        assert!(is_number_menu("1-5 ^3", WORDS));
        assert!(is_number_menu("  4  ", WORDS));
        assert!(is_number_menu("", WORDS));
        assert!(is_number_menu("   ", WORDS));
    }

    #[test]
    fn listed_names_are_not_natural_language() {
        assert!(is_number_menu("core", WORDS));
        assert!(is_number_menu("^aur", WORDS));
        assert!(is_number_menu("1-3 core", WORDS));
    }

    #[test]
    fn prose_is_natural_language() {
        assert!(!is_number_menu("the browser with drm support", WORDS));
        assert!(!is_number_menu("装一个浏览器", WORDS));
        assert!(!is_number_menu("1 and also a video player", WORDS));
        assert!(!is_number_menu("firefox", WORDS));
        assert!(!is_number_menu("what is the difference?", WORDS));
    }

    #[test]
    fn malformed_ranges_are_natural_language() {
        assert!(!is_number_menu("1-", WORDS));
        assert!(!is_number_menu("-3", WORDS));
        assert!(!is_number_menu("1-a", WORDS));
    }
}
