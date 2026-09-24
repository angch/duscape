//! Just enough of an xkb keymap to turn Wayland's key codes into keysyms: the compositor sends
//! the keymap as text in xkbcommon's format, and this reads its `xkb_keycodes` (key names to
//! codes) and `xkb_symbols` (key names to the keysyms of the first group's first two levels).
//! Types, actions, compose and further groups are left to real xkbcommon, which would be a C
//! library this binary does not link. Keys the keymap does not settle fall back to a US layout
//! by evdev code, so the arrows and letters always work.

use ::std::collections::HashMap;

use crate::backend::{keys, level_keysym};

/// Keysyms by xkb keycode (evdev code + 8): unshifted and shifted, 0 for none.
pub struct Xkb {
    levels: HashMap<u32, (u32, u32)>,
}

impl Xkb {
    pub fn parse(text: &str) -> Xkb {
        let codes = keycodes(text);
        let mut levels = HashMap::new();
        for (name, syms) in symbols(text) {
            if let Some(&code) = codes.get(name.as_str()) {
                let plain = syms.first().copied().unwrap_or(0);
                let shifted = syms.get(1).copied().unwrap_or(0);
                // A key with a keysym this does not name (ß, a dead key) is kept as meaning
                // nothing, so it never falls back to the US key in its place.
                levels.insert(code, (plain, shifted));
            }
        }
        Xkb { levels }
    }

    /// Nothing known: every key by the US fallback.
    pub fn empty() -> Xkb {
        Xkb {
            levels: HashMap::new(),
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.levels.len()
    }

    /// The keysym for an evdev key code with these modifiers, or `None` for a key that means
    /// nothing here (a modifier, a media key).
    pub fn keysym(&self, evdev: u32, shift: bool, lock: bool) -> Option<u32> {
        let (plain, shifted) = self
            .levels
            .get(&(evdev + 8))
            .copied()
            .or_else(|| us_fallback(evdev))?;
        (plain != 0).then(|| level_keysym(plain, shifted, shift, lock))
    }
}

/// `xkb_keycodes { <AC01> = 38; alias <LatA> = <AC01>; }`: names to codes, aliases resolved.
fn keycodes(text: &str) -> HashMap<String, u32> {
    let Some(block) = section(text, "xkb_keycodes") else {
        return HashMap::new();
    };
    let mut codes = HashMap::new();
    let mut aliases = Vec::new();
    for statement in block.split(';') {
        let statement = statement.trim();
        if let Some(rest) = statement.strip_prefix("alias") {
            if let Some((from, to)) = rest.split_once('=') {
                aliases.push((name_of(from), name_of(to)));
            }
        } else if let Some((name, value)) = statement.split_once('=')
            && name.trim().starts_with('<')
            && let Ok(code) = value.trim().parse::<u32>()
        {
            codes.insert(name_of(name), code);
        }
    }
    for (from, to) in aliases {
        if let Some(&code) = codes.get(&to) {
            codes.insert(from, code);
        }
    }
    codes
}

/// `xkb_symbols { key <AC01> { [ a, A ] }; }`: each key's name and the keysyms of its first
/// level list, in order.
fn symbols(text: &str) -> Vec<(String, Vec<u32>)> {
    let Some(block) = section(text, "xkb_symbols") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut rest = block;
    while let Some(at) = rest.find("key ") {
        rest = &rest[at + 4..];
        let Some(open) = rest.find('{') else {
            break;
        };
        let name = name_of(&rest[..open]);
        let Some(body_len) = matching_brace(&rest[open..]) else {
            break;
        };
        let body = &rest[open + 1..open + body_len - 1];
        rest = &rest[open + body_len..];
        // The first `[ ... ]` list is the first group's levels — not the index in
        // `symbols[Group1]=`, which is a bracket after a name rather than after `{`, `=` or `,`.
        if let Some(start) = level_list(body)
            && let Some(end) = body[start..].find(']')
        {
            let syms: Vec<u32> = body[start + 1..start + end]
                .split(',')
                .map(|sym| keysym_from_name(sym.trim()))
                .collect();
            out.push((name, syms));
        }
    }
    out
}

/// Where the first keysym list opens in a key's body: a `[` preceded by `{`, `=` or `,`.
fn level_list(body: &str) -> Option<usize> {
    body.match_indices('[').find_map(|(at, _)| {
        // Nothing before it means it opens the body, right after the key's `{`.
        let before = body[..at].trim_end().chars().next_back();
        matches!(before, None | Some('{' | '=' | ',')).then_some(at)
    })
}

/// The text between the braces of the first `kind "..." { ... }` section.
fn section<'a>(text: &'a str, kind: &str) -> Option<&'a str> {
    let at = text.find(kind)?;
    let open = at + text[at..].find('{')?;
    let len = matching_brace(&text[open..])?;
    Some(&text[open + 1..open + len - 1])
}

/// The length of the text from the `{` at the start of `text` to its matching `}`, inclusive.
fn matching_brace(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (index, ch) in text.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// `<AC01>` from around it, without the angle brackets.
fn name_of(text: &str) -> String {
    text.trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_string()
}

/// A keysym from its xkb name: a Latin-1 character's own name, or one of the keys the viewer
/// uses. Anything else is `0` (NoSymbol) — the viewer does nothing with it anyway.
fn keysym_from_name(name: &str) -> u32 {
    let mut chars = name.chars();
    if let (Some(ch), None) = (chars.next(), chars.next())
        && (ch as u32) < 0x100
    {
        return u32::from(ch);
    }
    match name {
        "space" => 0x20,
        "exclam" => 0x21,
        "quotedbl" => 0x22,
        "numbersign" => 0x23,
        "dollar" => 0x24,
        "percent" => 0x25,
        "ampersand" => 0x26,
        "apostrophe" => 0x27,
        "parenleft" => 0x28,
        "parenright" => 0x29,
        "asterisk" => 0x2a,
        "plus" => 0x2b,
        "comma" => 0x2c,
        "minus" => 0x2d,
        "period" => 0x2e,
        "slash" => 0x2f,
        "colon" => 0x3a,
        "semicolon" => 0x3b,
        "less" => 0x3c,
        "equal" => 0x3d,
        "greater" => 0x3e,
        "question" => 0x3f,
        "at" => 0x40,
        "bracketleft" => 0x5b,
        "backslash" => 0x5c,
        "bracketright" => 0x5d,
        "asciicircum" => 0x5e,
        "underscore" => 0x5f,
        "grave" => 0x60,
        "braceleft" => 0x7b,
        "bar" => 0x7c,
        "braceright" => 0x7d,
        "asciitilde" => 0x7e,
        "BackSpace" => keys::BACKSPACE,
        "Tab" => keys::TAB,
        "ISO_Left_Tab" => keys::ISO_LEFT_TAB,
        "Return" => keys::RETURN,
        "Escape" => keys::ESCAPE,
        "Home" => keys::HOME,
        "Left" => keys::LEFT,
        "Up" => keys::UP,
        "Right" => keys::RIGHT,
        "Down" => keys::DOWN,
        "Prior" | "Page_Up" => keys::PAGE_UP,
        "Next" | "Page_Down" => keys::PAGE_DOWN,
        "End" => keys::END,
        "Delete" => keys::DELETE,
        "KP_Enter" => keys::KP_ENTER,
        "KP_Home" => keys::KP_HOME,
        "KP_Left" => keys::KP_LEFT,
        "KP_Up" => keys::KP_UP,
        "KP_Right" => keys::KP_RIGHT,
        "KP_Down" => keys::KP_DOWN,
        "KP_Prior" | "KP_Page_Up" => keys::KP_PAGE_UP,
        "KP_Next" | "KP_Page_Down" => keys::KP_PAGE_DOWN,
        "KP_End" => keys::KP_END,
        "KP_Insert" => keys::KP_INSERT,
        "KP_Delete" => keys::KP_DELETE,
        "KP_Add" => keys::KP_ADD,
        "KP_Subtract" => keys::KP_SUBTRACT,
        "KP_0" => keys::KP_0,
        "F5" => keys::F5,
        _ => 0,
    }
}

/// A US keyboard by evdev code, for keys the keymap left out.
fn us_fallback(evdev: u32) -> Option<(u32, u32)> {
    const ROW_DIGITS: &[u8] = b"1234567890-=";
    const ROW_DIGITS_SHIFT: &[u8] = b"!@#$%^&*()_+";
    const ROW_Q: &[u8] = b"qwertyuiop[]";
    const ROW_A: &[u8] = b"asdfghjkl;'`";
    const ROW_Z: &[u8] = b"zxcvbnm,./";
    let letter = |ch: u8| (u32::from(ch), 0);
    Some(match evdev {
        1 => (keys::ESCAPE, 0),
        2..=13 => {
            let index = (evdev - 2) as usize;
            (
                u32::from(ROW_DIGITS[index]),
                u32::from(ROW_DIGITS_SHIFT[index]),
            )
        }
        14 => (keys::BACKSPACE, 0),
        15 => (keys::TAB, keys::ISO_LEFT_TAB),
        16..=27 => letter(ROW_Q[(evdev - 16) as usize]),
        28 => (keys::RETURN, 0),
        30..=41 => letter(ROW_A[(evdev - 30) as usize]),
        43 => (u32::from(b'\\'), u32::from(b'|')),
        44..=53 => letter(ROW_Z[(evdev - 44) as usize]),
        57 => (0x20, 0),
        63 => (keys::F5, 0),
        74 => (keys::KP_SUBTRACT, 0),
        78 => (keys::KP_ADD, 0),
        82 => (keys::KP_INSERT, keys::KP_0),
        96 => (keys::KP_ENTER, 0),
        102 => (keys::HOME, 0),
        103 => (keys::UP, 0),
        104 => (keys::PAGE_UP, 0),
        105 => (keys::LEFT, 0),
        106 => (keys::RIGHT, 0),
        107 => (keys::END, 0),
        108 => (keys::DOWN, 0),
        109 => (keys::PAGE_DOWN, 0),
        111 => (keys::DELETE, 0),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEYMAP: &str = r#"xkb_keymap {
xkb_keycodes "evdev+aliases(qwerty)" {
    minimum = 8;
    maximum = 255;
    <ESC>  = 9;
    <AE01> = 10;
    <AE11> = 20;
    <AD01> = 24;
    <AC01> = 38;
    <LEFT> = 113;
    <KPEN> = 104;
    alias <LatA> = <AC01>;
    indicator 1 = "Caps Lock";
};
xkb_types "complete" { type "ONE_LEVEL" { modifiers= none; level_name[Level1]= "Any"; }; };
xkb_compatibility "complete" { interpret.useModMapMods= AnyLevel; };
xkb_symbols "pc+de+inet(evdev)" {
    name[group1]="German";
    key  <ESC> {         [          Escape ] };
    key <AE01> {         [               1,          exclam,     onesuperior,      exclamdown ] };
    key <AE11> {         [          ssharp,        question,       backslash,    questiondown ] };
    key <AD01> {
        type= "FOUR_LEVEL",
        symbols[Group1]= [               q,               Q,              at,     Greek_OMEGA ]
    };
    key <LatA> {         [               a,               A,              ae,              AE ] };
    key <LEFT> {         [            Left ] };
    key <KPEN> {         [        KP_Enter ] };
    modifier_map Shift { <LFSH>, <RTSH> };
};
xkb_geometry "pc(pc105)" { width= 470; };
};"#;

    #[test]
    fn keycodes_symbols_and_aliases_are_read() {
        let xkb = Xkb::parse(KEYMAP);
        assert_eq!(xkb.len(), 7);
        assert_eq!(xkb.keysym(1, false, false), Some(keys::ESCAPE));
        assert_eq!(xkb.keysym(2, false, false), Some(u32::from('1')));
        assert_eq!(xkb.keysym(2, true, false), Some(u32::from('!')));
        assert_eq!(xkb.keysym(16, false, false), Some(u32::from('q')));
        assert_eq!(xkb.keysym(16, true, false), Some(u32::from('Q')));
        assert_eq!(
            xkb.keysym(30, false, true),
            Some(u32::from('A')),
            "through the alias"
        );
        assert_eq!(xkb.keysym(105, true, false), Some(keys::LEFT));
        assert_eq!(xkb.keysym(96, false, false), Some(keys::KP_ENTER));
        // ß has no name this reads, so the key means nothing rather than the US `-`.
        assert_eq!(xkb.keysym(12, false, false), None);
    }

    #[test]
    fn a_real_keymap_from_xkbcomp_is_read() {
        // Dumped with `xkbcomp -xkb $DISPLAY`: the format a compositor sends (xkb_v1).
        let xkb = Xkb::parse(include_str!("xkb_test_keymap.xkb"));
        assert!(xkb.len() > 100, "{} keys", xkb.len());
        assert_eq!(xkb.keysym(30, false, false), Some(u32::from('a')));
        assert_eq!(xkb.keysym(30, true, false), Some(u32::from('A')));
        assert_eq!(xkb.keysym(13, false, false), Some(u32::from('=')));
        assert_eq!(xkb.keysym(13, true, false), Some(u32::from('+')));
        assert_eq!(xkb.keysym(103, false, false), Some(keys::UP));
        assert_eq!(xkb.keysym(104, false, false), Some(keys::PAGE_UP));
        assert_eq!(xkb.keysym(111, false, false), Some(keys::DELETE));
        assert_eq!(xkb.keysym(15, true, false), Some(keys::ISO_LEFT_TAB));
    }

    #[test]
    fn keys_the_keymap_lacks_fall_back_to_a_us_layout() {
        let xkb = Xkb::empty();
        assert_eq!(xkb.keysym(32, false, false), Some(u32::from('d')));
        assert_eq!(xkb.keysym(32, true, false), Some(u32::from('D')));
        assert_eq!(xkb.keysym(13, true, false), Some(u32::from('+')));
        assert_eq!(xkb.keysym(108, false, false), Some(keys::DOWN));
        assert_eq!(xkb.keysym(28, false, false), Some(keys::RETURN));
        assert_eq!(xkb.keysym(42, false, false), None, "Shift itself");
    }
}
