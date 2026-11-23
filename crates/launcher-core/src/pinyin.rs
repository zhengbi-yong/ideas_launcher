//! Pinyin matching for CJK queries.
//!
//! Flow Launcher ships `PinyinAlphabet` in `Flow.Launcher.Infrastructure.dll`
//! with a double-pinyin table (XiaoHe schema), a polyphonic phrase override
//! table and a translation cache. When the query contains no Chinese
//! characters, the *haystack* is translated to pinyin and matched against
//! that, so `chongqi` finds "重启".
//!
//! The implementation here differs in one important way: the translation is
//! done **once, when the index is built**, not once per query and candidate.
//! Flow Launcher translates inside `FuzzyMatch`, which means a pinyin
//! conversion for every candidate of every keystroke; here the pinyin form
//! lives next to the original text and the matcher simply tries both.
//!
//! The polyphonic phrase overrides of the original *are* reproduced — but as
//! a hand-curated table instead of Flow Launcher's 2.7 MB one. The table
//! lives in [`crate::polyphone`] and is consulted before the single-character
//! dictionary, so "重启" resolves to "chong qi" and not to the dictionary's
//! "zhong qi".
//!
//! The table currently holds a few hundred entries covering the frequent
//! everyday ambiguities (重/行/长/乐/还/为/发/得/地/着/差/藏/朝/处/传/当/…),
//! each entry spelling one tone-less syllable per character, so the highlight
//! mapping stays exact. Maintenance rules and the full invariant list are
//! documented in [`crate::polyphone`].

/// The pinyin rendering of a text, together with a map back onto the source.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PinyinIndex {
    /// The pinyin rendering of the source text.
    pub pinyin: String,
    /// For every byte of `pinyin`, the byte range of the source character it
    /// came from. Used to map a pinyin hit back onto the original text.
    pub source_ranges: Vec<(usize, usize)>,
}

impl PinyinIndex {
    /// Build a pinyin rendering of `text`.
    ///
    /// Returns `None` when the text contains no CJK characters, so callers can
    /// skip the extra work entirely for the common ASCII case.
    ///
    /// Polyphonic words are resolved through the phrase table in
    /// [`crate::polyphone`]: the longest matching phrase wins and each of its
    /// syllables is mapped back onto its own character, so the highlighting
    /// stays as precise as for single characters.
    pub fn build(text: &str) -> Option<Self> {
        #[cfg(feature = "pinyin")]
        {
            use crate::polyphone;
            use pinyin::ToPinyin;

            let mut pinyin = String::with_capacity(text.len() * 2);
            let mut source_ranges: Vec<(usize, usize)> = Vec::with_capacity(text.len() * 2);
            let mut any_cjk = false;

            let mut chars = text.char_indices();
            while let Some((byte, ch)) = chars.next() {
                let end = byte + ch.len_utf8();
                if !is_cjk(ch) {
                    // The rendering is lower-cased, so the matcher never has
                    // to make a copy of it.
                    for lowered in ch.to_lowercase() {
                        for _ in 0..lowered.len_utf8() {
                            pinyin.push(lowered);
                            source_ranges.push((byte, end));
                        }
                    }
                    continue;
                }

                any_cjk = true;

                // A phrase override wins over the single-character dictionary:
                // "重启" is "chong qi", not "zhong qi".
                if let Some(entry) = polyphone::lookup_at(text, byte) {
                    let mut src = byte;
                    for (ch, syllable) in entry.phrase.chars().zip(entry.syllables.iter().copied())
                    {
                        let ch_end = src + ch.len_utf8();
                        pinyin.push_str(syllable);
                        for _ in 0..syllable.len() {
                            source_ranges.push((src, ch_end));
                        }
                        src = ch_end;
                    }
                    // Skip the characters the override has covered.
                    let mut left = src.saturating_sub(end);
                    while left > 0 {
                        match chars.next() {
                            Some((_, covered)) => left = left.saturating_sub(covered.len_utf8()),
                            None => break,
                        }
                    }
                    continue;
                }

                if let Some(py) = ch.to_pinyin().map(|p| p.plain()) {
                    // The plain rendering may contain a multi-byte "ü"; the
                    // bytes are copied verbatim so the mapping below stays
                    // one range per byte.
                    pinyin.push_str(py);
                    for _ in 0..py.len() {
                        source_ranges.push((byte, end));
                    }
                }
            }

            if !any_cjk {
                return None;
            }
            Some(Self {
                pinyin,
                source_ranges,
            })
        }

        #[cfg(not(feature = "pinyin"))]
        {
            let _ = text;
            None
        }
    }

    /// The pinyin rendering, already lower-cased.
    pub fn pinyin_lower(&self) -> &str {
        &self.pinyin
    }

    /// Map a byte range of the pinyin rendering back onto the source text.
    ///
    /// Returns `None` when the range is outside the rendering.
    pub fn map_range(&self, start: usize, end: usize) -> Option<(usize, usize)> {
        if start >= end {
            return None;
        }
        let first = self.source_ranges.get(start)?;
        let last = self.source_ranges.get(end - 1)?;
        Some((first.0, last.1))
    }

    /// Map every matched byte range back onto the source text.
    pub fn map_ranges(&self, ranges: &[(usize, usize)]) -> Vec<(usize, usize)> {
        let mut out = Vec::with_capacity(ranges.len());
        for (start, end) in ranges {
            match self.map_range(*start, *end) {
                Some(mapped) => out.push(mapped),
                None => out.push((*start, *end)),
            }
        }
        out
    }
}

/// True for CJK ideographs.
pub fn is_cjk(ch: char) -> bool {
    matches!(ch as u32,
        0x3400..=0x4DBF |   // CJK Extension A
        0x4E00..=0x9FFF |   // CJK Unified Ideographs
        0xF900..=0xFAFF |   // CJK Compatibility Ideographs
        0x20000..=0x2A6DF) // CJK Extension B
}

/// True when the text contains at least one CJK ideograph.
pub fn contains_cjk(text: &str) -> bool {
    text.chars().any(is_cjk)
}

/// The accent-stripped form of a character.
///
/// Mirrors Flow Launcher's `DiacriticsNormalizer`, which lets a user type
/// "cafe" and find "Café".
pub fn strip_accents(ch: char) -> char {
    let mut buf = [0u8; 4];
    let lower = ch.to_lowercase().collect::<String>();
    for (i, b) in lower.as_bytes().iter().enumerate() {
        buf[i] = *b;
    }
    match std::str::from_utf8(&buf[..lower.len()]) {
        Ok(s) => {
            let folded: String = s
                .chars()
                .map(|c| match c {
                    'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'ā' => 'a',
                    'é' | 'è' | 'ê' | 'ë' | 'ē' => 'e',
                    'í' | 'ì' | 'î' | 'ï' | 'ī' => 'i',
                    'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ō' => 'o',
                    'ú' | 'ù' | 'û' | 'ü' | 'ū' => 'u',
                    'ý' | 'ÿ' => 'y',
                    'ñ' => 'n',
                    'ç' => 'c',
                    'ß' => 's',
                    other => other,
                })
                .collect();
            folded.chars().next().unwrap_or(ch)
        }
        Err(_) => ch,
    }
}

/// The accent-stripped form of a whole string.
pub fn strip_accents_str(text: &str) -> String {
    if text.is_ascii() {
        return text.to_string();
    }
    text.chars().map(strip_accents).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_cjk() {
        assert!(is_cjk('雨'));
        assert!(!is_cjk('a'));
        assert!(contains_cjk("雨书"));
        assert!(!contains_cjk("chrome"));
    }

    #[cfg(feature = "pinyin")]
    #[test]
    fn builds_pinyin_for_cjk() {
        let idx = PinyinIndex::build("雨书").expect("should build");
        let lower = idx.pinyin_lower();
        assert!(lower.contains("yu"), "{lower}");
        assert!(lower.contains("shu"), "{lower}");
    }

    #[cfg(feature = "pinyin")]
    #[test]
    fn ascii_only_yields_none() {
        assert!(PinyinIndex::build("chrome").is_none());
    }

    #[cfg(feature = "pinyin")]
    #[test]
    fn pinyin_hit_maps_back_to_the_source() {
        let idx = PinyinIndex::build("雨书").expect("should build");
        let lower = idx.pinyin_lower();
        let start = lower.find("yu").expect("yu should be there");
        let (s, e) = idx.map_range(start, start + 2).expect("should map");
        assert_eq!(&"雨书"[s..e], "雨");
    }

    #[test]
    fn strips_accents() {
        assert_eq!(strip_accents_str("Café"), "cafe");
        assert_eq!(strip_accents_str("naïve"), "naive");
        assert_eq!(strip_accents_str("plain"), "plain");
    }
}
