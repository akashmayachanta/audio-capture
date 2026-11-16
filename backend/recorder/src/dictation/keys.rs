//! Key identities for the dictation gestures, as Raw Input reports them.
//! A key is named (`RightAlt`, `F13`, ...) or, for anything else, `vk:0xNN`.

use std::fmt;

/// Names that are not `F1`..`F24`.
const NAMED: &[&str] = &[
    "LeftCtrl", "RightCtrl", "LeftAlt", "RightAlt", "LeftShift", "RightShift", "LeftWin", "RightWin", "CapsLock",
    "ScrollLock", "Pause", "Insert", "Menu", "Escape",
];

/// `RAWKEYBOARD.MakeCode` of the Left Ctrl that Windows injects before AltGr's Right Alt.
const FAKE_CTRL_MAKE_CODE: u16 = 0x21D;
/// `RAWKEYBOARD.Flags` bit: the key is an extended (E0) key, e.g. the right-hand Ctrl/Alt.
const RI_KEY_E0: u16 = 0x02;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyId(String);

impl KeyId {
    /// A configured key name (case-insensitive); empty or unknown → `None`.
    pub fn parse(s: &str) -> Option<KeyId> {
        let s = s.trim();
        if let Some(hex) = s.to_ascii_lowercase().strip_prefix("vk:0x") {
            let v = u16::from_str_radix(hex, 16).ok()?;
            return (1..=0xFE).contains(&v).then(|| Self::vk(v));
        }
        if let Some(name) = NAMED.iter().find(|n| n.eq_ignore_ascii_case(s)) {
            return Some(KeyId((*name).into()));
        }
        let n: u8 = s.strip_prefix(['f', 'F'])?.parse().ok()?;
        (1..=24).contains(&n).then(|| KeyId(format!("F{n}")))
    }

    /// From a Raw Input keyboard event; `None` for events that are not a user key
    /// (the fake AltGr Ctrl, overrun markers).
    pub fn from_raw(vkey: u16, make_code: u16, flags: u16) -> Option<KeyId> {
        if make_code == FAKE_CTRL_MAKE_CODE || vkey == 0 || vkey == 0xFF {
            return None;
        }
        let e0 = flags & RI_KEY_E0 != 0;
        let name = match vkey {
            0x10 => if make_code == 0x36 { "RightShift" } else { "LeftShift" },
            0x11 => if e0 { "RightCtrl" } else { "LeftCtrl" },
            0x12 => if e0 { "RightAlt" } else { "LeftAlt" },
            0x5B => "LeftWin",
            0x5C => "RightWin",
            0x14 => "CapsLock",
            0x91 => "ScrollLock",
            0x13 => "Pause",
            0x2D => "Insert",
            0x5D => "Menu",
            0x1B => "Escape",
            0x70..=0x87 => return Some(KeyId(format!("F{}", vkey - 0x6F))),
            other => return Some(Self::vk(other)),
        };
        Some(KeyId(name.into()))
    }

    /// For the Settings page: "Right Alt", "Caps Lock", "Key 0x45".
    pub fn display_name(&self) -> String {
        if let Some(code) = self.0.strip_prefix("vk:") {
            return format!("Key {code}");
        }
        let mut out = String::new();
        for (i, c) in self.0.chars().enumerate() {
            if i > 0 && c.is_ascii_uppercase() {
                out.push(' ');
            }
            out.push(c);
        }
        out
    }

    fn vk(v: u16) -> KeyId {
        KeyId(format!("vk:0x{v:02X}"))
    }
}

impl fmt::Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const E0: u16 = 0x02;

    fn id(s: &str) -> KeyId {
        KeyId::parse(s).unwrap()
    }

    #[test]
    fn parses_names_case_insensitively() {
        assert_eq!(KeyId::parse("rightalt"), KeyId::parse("RightAlt"));
        assert_eq!(KeyId::parse("  f13 ").unwrap().to_string(), "F13");
    }

    #[test]
    fn empty_and_unknown_do_not_parse() {
        assert_eq!(KeyId::parse(""), None);
        assert_eq!(KeyId::parse("  "), None);
        assert_eq!(KeyId::parse("banana"), None);
        assert_eq!(KeyId::parse("vk:0xZZ"), None);
    }

    #[test]
    fn raw_modifiers_tell_left_from_right() {
        assert_eq!(KeyId::from_raw(0x12, 0x38, E0), Some(id("RightAlt")));
        assert_eq!(KeyId::from_raw(0x12, 0x38, 0), Some(id("LeftAlt")));
        assert_eq!(KeyId::from_raw(0x11, 0x1D, E0), Some(id("RightCtrl")));
        assert_eq!(KeyId::from_raw(0x11, 0x1D, 0), Some(id("LeftCtrl")));
        // Shift has no E0 bit: the make code says which one.
        assert_eq!(KeyId::from_raw(0x10, 0x36, 0), Some(id("RightShift")));
        assert_eq!(KeyId::from_raw(0x10, 0x2A, 0), Some(id("LeftShift")));
        assert_eq!(KeyId::from_raw(0x5B, 0x5B, E0), Some(id("LeftWin")));
        assert_eq!(KeyId::from_raw(0x5C, 0x5C, E0), Some(id("RightWin")));
    }

    #[test]
    fn the_fake_ctrl_before_altgr_is_ignored() {
        // Windows sends a Left Ctrl with make code 0x21D just before AltGr's Right Alt.
        assert_eq!(KeyId::from_raw(0x11, 0x21D, 0), None);
    }

    #[test]
    fn raw_function_and_lock_keys() {
        assert_eq!(KeyId::from_raw(0x70, 0x3B, 0), Some(id("F1")));
        assert_eq!(KeyId::from_raw(0x87, 0x76, 0), Some(id("F24")));
        assert_eq!(KeyId::from_raw(0x14, 0x3A, 0), Some(id("CapsLock")));
        assert_eq!(KeyId::from_raw(0x91, 0x46, 0), Some(id("ScrollLock")));
        assert_eq!(KeyId::from_raw(0x13, 0x45, 0), Some(id("Pause")));
        assert_eq!(KeyId::from_raw(0x5D, 0x5D, E0), Some(id("Menu")));
        assert_eq!(KeyId::from_raw(0x1B, 0x01, 0), Some(id("Escape")));
    }

    #[test]
    fn other_keys_keep_their_virtual_key_code() {
        let e = KeyId::from_raw(0x45, 0x12, 0).unwrap();
        assert_eq!(e.to_string(), "vk:0x45");
        assert_eq!(KeyId::parse("vk:0x45"), Some(e));
    }

    #[test]
    fn overrun_and_empty_codes_are_ignored() {
        assert_eq!(KeyId::from_raw(0xFF, 0, 0), None);
        assert_eq!(KeyId::from_raw(0, 0, 0), None);
    }

    #[test]
    fn every_named_key_round_trips() {
        for name in ["LeftCtrl", "RightCtrl", "LeftAlt", "RightAlt", "LeftShift", "RightShift", "LeftWin", "RightWin",
            "CapsLock", "ScrollLock", "Pause", "Insert", "Menu", "Escape", "F1", "F12", "F24", "vk:0x7F"]
        {
            let k = id(name);
            assert_eq!(KeyId::parse(&k.to_string()), Some(k), "{name}");
        }
    }

    #[test]
    fn display_names_are_readable() {
        assert_eq!(id("RightAlt").display_name(), "Right Alt");
        assert_eq!(id("CapsLock").display_name(), "Caps Lock");
        assert_eq!(id("F13").display_name(), "F13");
        assert_eq!(id("vk:0x45").display_name(), "Key 0x45");
    }
}
