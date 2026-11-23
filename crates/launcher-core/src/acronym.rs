//! Acronym (initialism) matching.
//!
//! Flow Launcher lets a user type `vs` to reach "Visual Studio" or `fld` to
//! reach "Flow Launcher Development". The upstream implementation lives in
//! `StringMatcher` (`Flow.Launcher.Infrastructure.dll`) and exposes
//! `IsAcronym`, `IsAcronymChar`, `IsAcronymCount` and
//! `CountDistinctAcronymGroups`.
//!
//! This module re-implements that behaviour with an explicit, documented
//! scoring model.

/// Compare two characters without regard to case.
///
/// The obvious `a.to_lowercase().eq(b.to_lowercase())` builds two iterator
/// chains per comparison, and this runs for every character of every word of
/// every candidate of every keystroke. ASCII is the common case and can be
/// compared with two instructions; anything else falls back to the general
/// comparison.
#[inline]
fn eq_ignore_case(a: char, b: char) -> bool {
    if a == b {
        return true;
    }
    if a.is_ascii() && b.is_ascii() {
        return a.to_ascii_lowercase() == b.to_ascii_lowercase();
    }
    a.to_lowercase().eq(b.to_lowercase())
}

/// A word inside a candidate title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    /// Byte offset of the word inside the original string.
    pub start: usize,
    /// Byte offset one past the end of the word.
    pub end: usize,
}

impl Word {
    /// The word's text.
    pub fn text<'a>(&self, haystack: &'a str) -> &'a str {
        &haystack[self.start..self.end]
    }

    /// The word's first character.
    pub fn initial(&self, haystack: &str) -> char {
        haystack[self.start..self.end].chars().next().unwrap_or('\0')
    }
}

/// Split a string into words.
///
/// A word boundary is any of: whitespace, `_`, `-`, `.`, `/`, `\`, `:`, `,`,
/// `(`, `)`, `[`, `]`, `{`, `}`, `+`, `&`, `'`, `"`, `!`, `?`, `@`, `#`, `*`,
/// or a lower→upper case transition (camelCase).
pub fn split_words(text: &str) -> Vec<Word> {
    let mut words = Vec::new();
    split_words_into(text, &mut words);
    words
}

/// Split a string into words, reusing the given buffer.
///
/// The matching hot path splits the same haystack twice — once for the
/// acronym match and once for the word-prefix match — and then once more for
/// the score. Reusing the buffer keeps that free of allocations.
pub fn split_words_into(text: &str, out: &mut Vec<Word>) {
    out.clear();
    let mut start: Option<usize> = None;
    let mut prev_lower = false;

    for (idx, ch) in text.char_indices() {
        let is_sep = ch.is_whitespace() || "_-./\\:,()[]{}!&?@#*'\"".contains(ch);
        let starts_new = !is_sep
            && start.is_some()
            && prev_lower
            && ch.is_uppercase();

        if is_sep || starts_new {
            if let Some(s) = start.take() {
                out.push(Word { start: s, end: idx });
            }
            if !is_sep {
                start = Some(idx);
            }
        } else if start.is_none() {
            start = Some(idx);
        }
        prev_lower = ch.is_lowercase() || ch.is_numeric();
    }

    if let Some(s) = start {
        out.push(Word { start: s, end: text.len() });
    }
}

/// Outcome of an acronym match attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcronymMatch {
    /// Byte ranges in the haystack that were matched.
    pub matched: Vec<(usize, usize)>,
    /// Number of distinct word groups that contributed a character.
    pub groups: usize,
    /// True when every query character landed on a word initial.
    pub all_initials: bool,
}

/// Try to match `query` against the initials of `haystack`'s words.
///
/// Matching rules:
/// * each query character must match the initial of a *distinct* word,
/// * words must be consumed in order,
/// * the match may skip words, but the more groups it spans the better.
pub fn match_acronym(query: &str, haystack: &str) -> Option<AcronymMatch> {
    let q: Vec<char> = query.chars().collect();
    if q.is_empty() {
        return None;
    }
    let mut words = Vec::new();
    split_words_into(haystack, &mut words);
    let mut matched = Vec::new();
    let groups = match_acronym_in(&q, haystack, &words, &mut matched)?;
    Some(AcronymMatch {
        matched,
        groups,
        all_initials: true,
    })
}

/// Match the query against the initials of already-split words.
///
/// Returns the number of word groups that contributed a character; the
/// matched byte ranges are written into `matched`.
pub fn match_acronym_in(
    query: &[char],
    haystack: &str,
    words: &[Word],
    matched: &mut Vec<(usize, usize)>,
) -> Option<usize> {
    matched.clear();
    if query.is_empty() || words.is_empty() {
        return None;
    }

    // Greedy left-to-right assignment of query chars to word initials.
    let mut groups = 0usize;
    let mut qi = 0usize;

    for word in words {
        if qi >= query.len() {
            break;
        }
        let initial = word.initial(haystack);
        if eq_ignore_case(initial, query[qi]) {
            matched.push((word.start, word.start + initial.len_utf8()));
            groups += 1;
            qi += 1;
        }
    }

    if qi != query.len() {
        return None;
    }
    Some(groups)
}

/// Try to match `query` against a single word's leading characters.
///
/// This covers the "acronym count" case in Flow Launcher, where e.g. `npd`
/// reaches "Notepad" by taking leading letters of consecutive words in one
/// segment. It also allows matching the first character of the haystack
/// followed by characters inside the same word, which is what users expect
/// from a launcher (e.g. `chrome` -> `chrome`).
pub fn match_word_prefixes(query: &str, haystack: &str) -> Option<AcronymMatch> {
    let q: Vec<char> = query.chars().collect();
    if q.is_empty() {
        return None;
    }
    let mut words = Vec::new();
    split_words_into(haystack, &mut words);
    let mut matched = Vec::new();
    let mut offsets = Vec::new();
    let groups = match_word_prefixes_in(&q, haystack, &words, &mut matched, &mut offsets)?;
    Some(AcronymMatch {
        matched,
        groups,
        all_initials: true,
    })
}

/// Match the query against the leading characters of already-split words.
///
/// Returns the number of word groups that contributed a character; the
/// matched byte ranges are written into `matched`.
pub fn match_word_prefixes_in(
    query: &[char],
    haystack: &str,
    words: &[Word],
    matched: &mut Vec<(usize, usize)>,
    offsets: &mut Vec<(usize, usize)>,
) -> Option<usize> {
    matched.clear();
    if query.is_empty() || words.is_empty() {
        return None;
    }

    // Consume the query across word prefixes: each word contributes a run of
    // matching leading characters, and words are consumed in order.
    let mut qi = 0usize;
    let mut groups = 0usize;

    for word in words {
        if qi >= query.len() {
            break;
        }
        let word_text = word.text(haystack);
        offsets.clear();
        let mut cursor = word.start;

        for ch in word_text.chars() {
            if qi + offsets.len() >= query.len() {
                break;
            }
            if eq_ignore_case(ch, query[qi + offsets.len()]) {
                offsets.push((cursor, cursor + ch.len_utf8()));
                cursor += ch.len_utf8();
            } else {
                // Either this word does not start the remaining query, or the
                // run ends here. Both cases stop consuming this word.
                break;
            }
        }

        if offsets.is_empty() {
            continue;
        }
        qi += offsets.len();
        groups += 1;
        matched.extend_from_slice(offsets);
    }

    if qi != query.len() || matched.is_empty() {
        return None;
    }
    Some(groups)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_whitespace_and_case() {
        let words = split_words("Visual Studio Code");
        assert_eq!(words.len(), 3);
        assert_eq!(words[0].text("Visual Studio Code"), "Visual");
    }

    #[test]
    fn splits_camel_case() {
        let words = split_words("PowerShell");
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].text("PowerShell"), "Power");
        assert_eq!(words[1].text("PowerShell"), "Shell");
    }

    #[test]
    fn acronym_matches_initials() {
        let m = match_acronym("vsc", "Visual Studio Code").expect("should match");
        assert_eq!(m.groups, 3);
        assert_eq!(m.matched.len(), 3);
    }

    #[test]
    fn acronym_is_case_insensitive() {
        assert!(match_acronym("VSC", "Visual Studio Code").is_some());
    }

    #[test]
    fn acronym_rejects_wrong_order() {
        assert!(match_acronym("csv", "Visual Studio Code").is_none());
    }

    #[test]
    fn acronym_needs_enough_words() {
        assert!(match_acronym("vs", "Video").is_none());
    }

    #[test]
    fn word_prefix_match_consumes_words_in_order() {
        let m = match_word_prefixes("vs", "Visual Studio");
        assert!(m.is_some());
        assert_eq!(m.unwrap().groups, 2);
    }
}
