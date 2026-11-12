//! The company name's checks (BP-24a, demo-only; Breakpoint plan §4e rank 14, "Name your company":
//! at most 24 characters, a rude-word filter). Pure functions: `ops/name` calls `check_name` and
//! stores what it returns.
//!
//! The filter keeps booth screens clean. It is not the full game's corp-name filter (a launch
//! requirement, `Soulbound_NFT_Policy.md` §5). Known residuals, by design: look-alike letters from
//! other scripts (Cyrillic `а` for Latin `a`) pass; an accent typed as a combining mark (NFD, `e` +
//! U+0301) is refused by the character set, since normalising it would need a new crate; a rude
//! word split across two longer words (`f-uck`) passes; `mishit` is refused.

/// The longest name, in `char`s (not bytes) after the spaces are collapsed.
pub const DEMO_NAME_MAX_CHARS: usize = 24;

/// Refused when they are a whole word of the name (or a run of one-letter words spelled out).
/// For words that are innocent inside longer ones (Scunthorpe, Hancock, Raccoon, Penistone).
/// No entry contains an `ANYWHERE` entry, so each list decides alone.
pub const WHOLE_WORDS: &[&str] = &[
    "arse", "ass", "chink", "cock", "cocks", "coon", "cum", "cunt", "cunts", "dick", "dicks", "fag", "fags",
    "homo", "nazi", "nazis", "nigga", "niggas", "nigger", "niggers", "paki", "penis", "piss", "pissed",
    "prick", "pricks", "rape", "rapist", "rapists", "retard", "retarded", "retards", "spic", "tits",
    "tranny", "twat", "twats", "wank",
];

/// Refused anywhere inside a word. Only terms that no common English word contains (measured
/// against a 370,105-word list, 2026-11-05: each hits only its own forms, and `shit` also `mishit`).
pub const ANYWHERE: &[&str] = &[
    "arsehole", "asshole", "bitch", "bollock", "dickhead", "dildo", "faggot", "fuck", "hitler", "jizz",
    "kike", "porn", "shit", "slut", "wanker", "whore",
];

/// Digits read as the letters they imitate (`sh1t`); every other digit is a break between words.
const LOOKALIKES: [(char, char); 6] = [('0', 'o'), ('1', 'i'), ('3', 'e'), ('4', 'a'), ('5', 's'), ('7', 't')];

/// Why a name was refused, in the order `check_name` checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameRefusal {
    /// Empty, or over `DEMO_NAME_MAX_CHARS`, after the spaces are collapsed.
    Length,
    /// A character outside letters (any script), ASCII digits, the space and `-'.&`.
    Charset,
    /// No letter at all (`2026`, `- . &`).
    NoLetter,
    /// A word on `WHOLE_WORDS`, or one containing an `ANYWHERE` term.
    Rude,
}

impl NameRefusal {
    /// The `400` body's text.
    pub fn text(self) -> &'static str {
        match self {
            NameRefusal::Length => "a company name is 1 to 24 characters",
            NameRefusal::Charset => "letters, numbers, spaces and - ' . & only",
            NameRefusal::NoLetter => "a company name needs at least one letter",
            NameRefusal::Rude => "HR did not appreciate that name. What did I just say about being nice?",
        }
    }
}

/// The name to store, or why not. Trims and collapses every run of whitespace (tab, NBSP, U+3000
/// included) to one space, then checks in order: length (`chars()`), character set, a letter,
/// the word lists.
pub fn check_name(raw: &str) -> Result<String, NameRefusal> {
    let name = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let n = name.chars().count();
    if n == 0 || n > DEMO_NAME_MAX_CHARS {
        return Err(NameRefusal::Length);
    }
    if !name.chars().all(|c| allowed(c)) {
        return Err(NameRefusal::Charset);
    }
    if !name.chars().any(char::is_alphabetic) {
        return Err(NameRefusal::NoLetter);
    }
    if is_rude(&name) {
        return Err(NameRefusal::Rude);
    }
    Ok(name)
}

fn allowed(c: char) -> bool {
    c.is_alphabetic() || c.is_ascii_digit() || matches!(c, ' ' | '-' | '\'' | '.' | '&')
}

/// Lowercased, look-alike digits read as letters, split on everything that isn't a letter.
fn words(name: &str) -> Vec<String> {
    let folded: String = name
        .chars()
        .flat_map(char::to_lowercase)
        .map(|c| LOOKALIKES.iter().find(|(d, _)| *d == c).map_or(c, |(_, l)| *l))
        .collect();
    folded.split(|c: char| !c.is_alphabetic()).filter(|w| !w.is_empty()).map(str::to_string).collect()
}

/// The words, plus each run of two or more one-letter words joined (`f u c k`, `f.u.c.k`). Two
/// longer words are never joined, so `Push It` is not `pushit`.
fn candidates(name: &str) -> Vec<String> {
    let words = words(name);
    let mut out = words.clone();
    let mut run = String::new();
    let mut len = 0;
    for w in words.iter().map(String::as_str).chain(std::iter::once("")) {
        if w.chars().count() == 1 {
            run.push_str(w);
            len += 1;
            continue;
        }
        if len >= 2 {
            out.push(std::mem::take(&mut run));
        }
        run.clear();
        len = 0;
    }
    out
}

fn is_rude(name: &str) -> bool {
    candidates(name).iter().any(|w| WHOLE_WORDS.contains(&w.as_str()) || ANYWHERE.iter().any(|t| w.contains(t)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed, inner runs collapsed to one space; tab, NBSP and U+3000 are whitespace.
    #[test]
    fn name_is_trimmed_and_its_spaces_collapsed() {
        assert_eq!(check_name("  Acme   Mining  "), Ok("Acme Mining".to_string()));
        assert_eq!(check_name("Acme\u{00A0}Ore\tCo\u{3000}Ltd"), Ok("Acme Ore Co Ltd".to_string()));
    }

    /// 1 to 24 `char`s after collapsing: 24 three-byte letters pass, 25 don't; empty and
    /// all-space names are too short.
    #[test]
    fn name_length_counts_chars_not_bytes() {
        assert_eq!(check_name(&"日".repeat(24)), Ok("日".repeat(24)));
        assert_eq!(check_name(&"日".repeat(25)), Err(NameRefusal::Length));
        assert_eq!(check_name(&"a".repeat(24)), Ok("a".repeat(24)));
        assert_eq!(check_name(&"a".repeat(25)), Err(NameRefusal::Length));
        assert_eq!(check_name(""), Err(NameRefusal::Length));
        assert_eq!(check_name("   "), Err(NameRefusal::Length));
    }

    /// Letters of any script, ASCII digits, the space and `-'.&` pass; emoji, the zero-width
    /// space, a bidi override, `_` and a combining accent (the NFD residual) don't.
    #[test]
    fn name_charset_refuses_emoji_and_invisibles() {
        for ok in ["Ünïtëd Çörp & Sons", "O'Malley-Smith Ltd.", "日本ж 9"] {
            assert_eq!(check_name(ok), Ok(ok.to_string()), "{ok}");
        }
        for bad in ["Acme \u{1F525}", "Ac\u{200B}me", "\u{202E}Acme", "Acme_Co", "Acme e\u{301}"] {
            assert_eq!(check_name(bad), Err(NameRefusal::Charset), "{bad:?}");
        }
    }

    /// Digits and punctuation alone are not a name.
    #[test]
    fn name_needs_a_letter() {
        assert_eq!(check_name("2026"), Err(NameRefusal::NoLetter));
        assert_eq!(check_name("- . &"), Err(NameRefusal::NoLetter));
    }

    /// Each `WHOLE_WORDS` entry is refused as a word, in any case; none contains an `ANYWHERE`
    /// term, so this list alone decides each one.
    #[test]
    fn whole_words_are_refused_as_words() {
        for w in WHOLE_WORDS {
            assert!(!ANYWHERE.iter().any(|t| w.contains(t)), "{w} is also caught by ANYWHERE");
            assert_eq!(check_name(&format!("{} Co", w.to_uppercase())), Err(NameRefusal::Rude), "{w}");
        }
    }

    /// Each `ANYWHERE` term is refused inside a longer word and spelled out with spaces or dots.
    #[test]
    fn anywhere_terms_are_refused_inside_words_and_spelled_out() {
        for t in ANYWHERE {
            assert_eq!(check_name(&format!("Big{t}co")), Err(NameRefusal::Rude), "{t}");
            let spaced: String = t.chars().map(|c| c.to_string()).collect::<Vec<_>>().join(" ");
            assert_eq!(check_name(&spaced), Err(NameRefusal::Rude), "{spaced}");
            let dotted: String = t.chars().map(|c| c.to_string()).collect::<Vec<_>>().join(".");
            assert_eq!(check_name(&dotted), Err(NameRefusal::Rude), "{dotted}");
        }
    }

    /// Look-alike digits are read as letters.
    #[test]
    fn lookalike_digits_are_read_as_letters() {
        assert_eq!(check_name("Sh1t Mining"), Err(NameRefusal::Rude));
        assert_eq!(check_name("B1TCH3S Inc"), Err(NameRefusal::Rude));
    }

    /// Names with a listed word inside a longer innocent word, or across two words, pass.
    #[test]
    fn innocent_names_pass() {
        for ok in [
            "Scunthorpe Mining", "Hancock & Peacock", "Raccoon Logistics", "Grape Expectations", "Penistone Ore Co",
            "Dickens Drilling", "Therapist Union", "Class Act Ltd", "Cumbria Coal", "Spice Haulage", "Analyst Corp",
            "Push It Co", "Glass Hit Co", "Snigger & Sons", "Pakistan Ore Co", "Cocoon Salvage", "Arsenal Salvage",
            "Essex Exports", "Swanky Holdings", "Area 51 Mining", "A B C Mining",
        ] {
            assert_eq!(check_name(ok), Ok(ok.to_string()), "{ok}");
        }
    }

    /// Each refusal's text, as the route answers it.
    #[test]
    fn refusal_texts() {
        assert_eq!(NameRefusal::Length.text(), "a company name is 1 to 24 characters");
        assert_eq!(NameRefusal::Charset.text(), "letters, numbers, spaces and - ' . & only");
        assert_eq!(NameRefusal::NoLetter.text(), "a company name needs at least one letter");
        assert_eq!(NameRefusal::Rude.text(), "HR did not appreciate that name. What did I just say about being nice?");
    }
}
