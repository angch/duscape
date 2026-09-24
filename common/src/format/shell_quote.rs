//! Quoting paths so they can be pasted into a shell and mean exactly that path.
//!
//! The result is also shown on screen, so it must be safe to display as well as to paste: nothing
//! in it may be a control character, and nothing may reorder the text around it. A file name can
//! hold a newline, an escape sequence or a right-to-left override; pasted raw, the first would run
//! the line early and the second would reach the terminal. Those are written as escapes instead.

use ::std::path::Path;

/// Quote `path` for the shell people on this platform paste into: POSIX `sh` (bash, zsh, dash) on
/// Unix, PowerShell on Windows.
pub fn quote_path_for_shell(path: &Path) -> String {
    #[cfg(unix)]
    {
        use ::std::os::unix::ffi::OsStrExt;
        quote_posix(path.as_os_str().as_bytes())
    }
    #[cfg(windows)]
    {
        quote_powershell(&without_verbatim_prefix(&path.to_string_lossy()))
    }
    #[cfg(not(any(unix, windows)))]
    {
        quote_posix(path.to_string_lossy().as_bytes())
    }
}

/// Characters that mean the same inside and outside quotes in POSIX shells, in any position.
///
/// Deliberately short. `~` and `=` expand at the start of a word (`=ls` in zsh), `%` names a job,
/// `!` is history in interactive bash, and `[`, `*`, `?`, `{` glob; anything not listed here is
/// quoted, which is always correct and only sometimes necessary.
fn is_plain_posix(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(c, '_' | '-' | '.' | '/' | '+' | ',' | ':' | '@')
        || (!c.is_ascii() && c.is_alphanumeric())
}

/// Characters that change how text around them is displayed without being visible themselves:
/// bidirectional marks, overrides and isolates, zero-width characters, the line and paragraph
/// separators some terminals break lines on, and the byte-order mark. A name holding one can
/// show on screen as a different name than the one being copied.
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

/// Quote a path's raw bytes for a POSIX shell.
///
/// - Plain paths are returned as they are: `photos/2024/img.jpg`.
/// - Anything else is single-quoted, where nothing is special but `'` itself, written `'\''`:
///   `'my files/it'\''s here'`.
/// - A name holding a control character, an invisible formatting character or bytes that are not
///   UTF-8 is written in `$'…'` (ANSI-C quoting, in bash, zsh, ksh and recent dash), where each of
///   those is an escape: `$'line\nbreak'`, `$'bad\xff'`, `$'\xe2\x80\xaetxt.exe'`.
pub fn quote_posix(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "''".to_string();
    }
    match ::std::str::from_utf8(bytes) {
        Ok(text) if !text.chars().any(|c| c.is_control() || is_invisible(c)) => {
            if text.chars().all(is_plain_posix) {
                text.to_string()
            } else {
                format!("'{}'", text.replace('\'', r"'\''"))
            }
        }
        _ => ansi_c_quote(bytes),
    }
}

/// `$'…'` quoting: printable text as it is, everything else escaped. Invalid UTF-8 is written
/// byte by byte, so the path pastes back to exactly the bytes on disk.
fn ansi_c_quote(mut bytes: &[u8]) -> String {
    use ::std::fmt::Write;

    let mut quoted = String::from("$'");
    while !bytes.is_empty() {
        let (valid, rest) = match ::std::str::from_utf8(bytes) {
            Ok(text) => (text, &[][..]),
            Err(error) => {
                let (valid, rest) = bytes.split_at(error.valid_up_to());
                // Everything before `valid_up_to` is valid UTF-8 by definition.
                (::std::str::from_utf8(valid).unwrap_or_default(), rest)
            }
        };
        for c in valid.chars() {
            match c {
                '\\' => quoted.push_str(r"\\"),
                '\'' => quoted.push_str(r"\'"),
                '\n' => quoted.push_str(r"\n"),
                '\t' => quoted.push_str(r"\t"),
                '\r' => quoted.push_str(r"\r"),
                // Two hex digits always: `\x` takes at most two, so a following digit cannot join.
                c if c.is_ascii_control() => {
                    let _ = write!(quoted, r"\x{:02x}", c as u32);
                }
                // Written as its UTF-8 bytes rather than `\u`, which bash before 4.2 (macOS's
                // `/bin/bash` is 3.2) leaves as literal text.
                c if c.is_control() || is_invisible(c) => {
                    let mut utf8 = [0; 4];
                    for byte in c.encode_utf8(&mut utf8).bytes() {
                        let _ = write!(quoted, r"\x{byte:02x}");
                    }
                }
                c => quoted.push(c),
            }
        }
        // One byte that does not start valid UTF-8, then carry on after it.
        if let Some((&byte, after)) = rest.split_first() {
            let _ = write!(quoted, r"\x{byte:02x}");
            bytes = after;
        } else {
            bytes = rest;
        }
    }
    quoted.push('\'');
    quoted
}

/// Characters PowerShell reads literally outside quotes, in any position. Not `,`, which builds an
/// array, nor `@`, which splats.
fn is_plain_powershell(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(c, '_' | '-' | '.' | '\\' | '/' | ':' | '+')
        || (!c.is_ascii() && c.is_alphanumeric())
}

/// Quote a path for PowerShell.
///
/// Plain paths are returned as they are; anything else is single-quoted, the one PowerShell quote
/// in which `$`, `` ` `` and `%` are not special. A `'` inside is doubled — and so are `‘` and `’`,
/// which PowerShell also accepts as single quotes. Windows forbids control characters in names;
/// any that arrive anyway are written as `` `u{…} `` inside a double-quoted string, the only
/// PowerShell form that can express them.
pub fn quote_powershell(text: &str) -> String {
    if text.is_empty() {
        return "''".to_string();
    }
    if text.chars().any(|c| c.is_control() || is_invisible(c)) {
        let mut quoted = String::from("\"");
        for c in text.chars() {
            match c {
                // PowerShell also closes a double-quoted string on the typographic double quotes.
                '`' | '"' | '$' | '\u{201C}' | '\u{201D}' | '\u{201E}' => {
                    quoted.push('`');
                    quoted.push(c);
                }
                c if c.is_control() || is_invisible(c) => {
                    quoted.push_str(&format!("`u{{{:X}}}", c as u32));
                }
                c => quoted.push(c),
            }
        }
        quoted.push('"');
        return quoted;
    }
    if text.chars().all(is_plain_powershell) {
        return text.to_string();
    }
    let mut quoted = String::from("'");
    for c in text.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}') {
            quoted.push(c);
        }
        quoted.push(c);
    }
    quoted.push('\'');
    quoted
}

/// Drop the `\\?\` prefix `canonicalize` puts on Windows paths, which most programs do not
/// accept: `\\?\C:\x` is `C:\x`, and `\\?\UNC\server\share` is `\\server\share`.
pub fn without_verbatim_prefix(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        path.to_string()
    }
}

/// The path from `base` to `target`, both absolute: `/home/user/bar/baz` from `/home/user/foo` is
/// `../bar/baz`, and `base` itself is `.`.
///
/// Purely lexical, so both should already be resolved — canonical, or as `getcwd` reports — or a
/// symbolic link in either would make `..` lead somewhere else. `None` when there is no relative
/// path between them: a relative input, or on Windows a different drive or share.
pub fn relative_to(target: &Path, base: &Path) -> Option<::std::path::PathBuf> {
    use ::std::path::{Component, PathBuf};

    if !target.is_absolute() || !base.is_absolute() {
        return None;
    }
    let mut target_components = target.components().peekable();
    let mut base_components = base.components().peekable();
    // A different root — `C:` against `D:`, or another share — has no path between them.
    if let (Some(Component::Prefix(target_prefix)), Some(Component::Prefix(base_prefix))) =
        (target_components.peek(), base_components.peek())
        && target_prefix.kind() != base_prefix.kind()
    {
        return None;
    }
    while let (Some(left), Some(right)) = (target_components.peek(), base_components.peek()) {
        if left != right {
            break;
        }
        target_components.next();
        base_components.next();
    }
    let mut relative: PathBuf = base_components.map(|_| Component::ParentDir).collect();
    relative.extend(target_components);
    if relative.as_os_str().is_empty() {
        relative.push(".");
    }
    Some(relative)
}

/// Which a copied path turned out to be: [`copied_path`] falls back to absolute when there is no
/// relative path to be had.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    Relative,
    Absolute,
}

impl PathKind {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            PathKind::Relative => "relative",
            PathKind::Absolute => "absolute",
        }
    }
}

/// `full` as a viewer copies it, ready to paste after a command: relative to `working_dir`, or
/// absolute when `absolute` is asked for or there is no relative path (the working directory is
/// unknown, or on another drive), quoted for the platform's shell. A relative path starting with
/// `-` gets a `./`, since pasted after a command `-rf` is an option however it is quoted.
#[must_use]
pub fn copied_path(full: &Path, working_dir: Option<&Path>, absolute: bool) -> (PathKind, String) {
    let relative = (!absolute)
        .then(|| working_dir.and_then(|working_dir| relative_to(full, working_dir)))
        .flatten();
    let (kind, path) = match relative {
        Some(relative) if relative.as_os_str().as_encoded_bytes().first() == Some(&b'-') => {
            (PathKind::Relative, Path::new(".").join(relative))
        }
        Some(relative) => (PathKind::Relative, relative),
        None => (PathKind::Absolute, full.to_path_buf()),
    };
    (kind, quote_path_for_shell(&path))
}
