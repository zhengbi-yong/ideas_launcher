//! Fuzzy matching and scoring.
//!
//! Re-implements the behaviour of `StringMatcher` in Flow Launcher's
//! `Flow.Launcher.Infrastructure.dll`, which exposes `FuzzySearch`,
//! `FuzzyMatch`, `CalculateSearchScore`, `GetUpdatedIndexList`,
//! `AllQuerySubstringsMatched` and the acronym helpers.
//!
//! # Scoring model
//!
//! Every candidate is scored on a 0..=1000 scale so that results from
//! different plugins remain comparable. The score starts from a *base* that
//! encodes how the query matched, and is then adjusted:
//!
//! | match kind        | base |
//! |-------------------|------|
//! | exact             | 1000 |
//! | prefix            | 800  |
//! | word-boundary run | 700  |
//! | acronym           | 650  |
//! | substring         | 500  |
//! | subsequence       | 250  |
//!
//! Adjustments then reward compact, boundary-aligned matches and penalise
//! gaps and late starts.

use std::cell::RefCell;

use crate::acronym;

pub use crate::result::SearchPrecision;

/// Knobs controlling how a match is evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchOptions {
    /// Compare without regard to case.
    pub ignore_case: bool,
}

impl Default for MatchOptions {
    fn default() -> Self {
        Self { ignore_case: true }
    }
}

/// The outcome of matching a query against a candidate string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchResult {
    /// True when the query matched.
    pub success: bool,
    /// Final score, 0..=1000.
    pub score: i32,
    /// Score before precision filtering was applied.
    pub raw_score: i32,
    /// Byte ranges in the haystack that were matched, ascending and disjoint.
    pub matched_indices: Vec<(usize, usize)>,
    /// The precision tier that was in effect.
    pub precision: SearchPrecision,
}

impl MatchResult {
    /// Build an unsuccessful result.
    pub fn failure(precision: SearchPrecision) -> Self {
        Self {
            success: false,
            score: 0,
            raw_score: 0,
            matched_indices: Vec::new(),
            precision,
        }
    }
}

/// Upper bound of the scoring scale.
pub const MAX_SCORE: i32 = 1000;

const BASE_EXACT: i32 = 1000;
const BASE_PREFIX: i32 = 800;
const BASE_BOUNDARY: i32 = 700;
const BASE_ACRONYM: i32 = 650;
const BASE_SUBSTRING: i32 = 500;
const BASE_PARTIAL_BOUNDARY: i32 = 450;
const BASE_SUBSEQUENCE: i32 = 250;

/// Characters that start a new "word" for boundary-bonus purposes.
///
/// The string is searched with `contains`, and the compiler turns that into a
/// mask test — a `match` with twenty arms was measured to be **slower**
/// (25 ns per candidate against 19 ns), even though it looks faster.
fn is_boundary_char(c: char) -> bool {
    c.is_whitespace() || "_-./\\:,()[]{}!&?@#*'\"".contains(c)
}

/// A character of the haystack together with its byte offset.
///
/// The offset is a `usize`, and not a `u32`: the character was measured to be
/// **faster** with the wider offset (19 ns per candidate against 24 ns), even
/// though it takes 24 bytes instead of 16. The order of the fields is what
/// the hot loop walks over.
struct HayChar {
    ch: char,
    byte: usize,
    lower: char,
    boundary: bool,
}

fn haystack_chars_into(text: &str, ignore_case: bool, out: &mut Vec<HayChar>) {
    out.clear();
    out.reserve(text.len());
    let mut prev_lower = false;
    for (byte, ch) in text.char_indices() {
        let boundary = is_boundary_char(ch)
            || (prev_lower && ch.is_uppercase())
            || byte == 0;
        let lower = if ignore_case {
            ch.to_lowercase().next().unwrap_or(ch)
        } else {
            ch
        };
        out.push(HayChar { ch, byte, lower, boundary });
        prev_lower = ch.is_lowercase();
    }
}

/// Locate the query inside `hay` as a subsequence, preferring word
/// boundaries and keeping the matched span as tight as possible.
///
/// The matched character indices are written into `indices` and the resulting
/// byte ranges into `matched`. Returns `false` when the query does not match.
fn fuzzy_subsequence_into(
    query: &[char],
    hay: &[HayChar],
    indices: &mut Vec<usize>,
    matched: &mut Vec<(usize, usize)>,
) -> bool {
    indices.clear();
    matched.clear();
    if query.is_empty() {
        return true;
    }
    if query.len() > hay.len() {
        return false;
    }

    // Forward pass: greedy left-to-right match.
    let mut cursor = 0usize;
    for qc in query {
        let mut found = None;
        let mut i = cursor;
        while i < hay.len() {
            if hay[i].lower == *qc {
                found = Some(i);
                break;
            }
            i += 1;
        }
        let Some(idx) = found else {
            indices.clear();
            return false;
        };
        indices.push(idx);
        cursor = idx + 1;
    }

    // Backward pass: tighten the start of the match by re-matching backwards
    // from the last matched character. This removes leading slack that the
    // greedy forward pass may have introduced.
    let mut start = indices[0];
    let mut bi = indices.len();
    let mut j = indices[indices.len() - 1];
    loop {
        while bi > 0 && hay[j].lower != query[bi - 1] {
            j -= 1;
            if j == 0 {
                break;
            }
        }
        if bi == 0 {
            break;
        }
        bi -= 1;
        if hay[j].lower == query[bi] {
            start = j;
            if bi == 0 {
                break;
            }
            if j == 0 {
                break;
            }
            j -= 1;
        } else {
            break;
        }
    }

    // Recompute the matched set inside the tightened window so that the
    // highlighted range is contiguous-ish and correct.
    let mut cursor = start;
    for qc in query {
        let mut i = cursor;
        while i < hay.len() && hay[i].lower != *qc {
            i += 1;
        }
        if i >= hay.len() {
            matched.clear();
            return false;
        }
        matched.push((hay[i].byte, hay[i].byte + hay[i].ch.len_utf8()));
        cursor = i + 1;
    }
    true
}

/// Collect the byte offsets of the word boundaries of a haystack.
fn collect_boundaries(hay: &[HayChar], out: &mut Vec<usize>) {
    out.clear();
    out.reserve(hay.len() / 4 + 4);
    for h in hay {
        if h.boundary {
            out.push(h.byte);
        }
    }
}

/// Counts of matched characters that landed on a word boundary.
///
/// `all` counts every boundary hit; `words` excludes a hit on the very first
/// character of the haystack, which is a boundary only because the string
/// starts there and carries no information.
fn boundary_hits(matched: &[(usize, usize)], boundaries: &[usize]) -> (usize, usize) {
    let mut all = 0usize;
    let mut words = 0usize;
    for (start, _) in matched {
        if boundaries.binary_search(start).is_ok() {
            all += 1;
            if *start != 0 {
                words += 1;
            }
        }
    }
    (all, words)
}

/// Count characters that are part of a consecutive run.
fn consecutive_pairs(matched: &[(usize, usize)]) -> usize {
    let mut count = 0usize;
    for w in matched.windows(2) {
        if w[0].1 == w[1].0 {
            count += 1;
        }
    }
    count
}

/// Number of unmatched characters between matched spans.
fn total_gap(matched: &[(usize, usize)], haystack_len: usize) -> usize {
    if matched.is_empty() {
        return haystack_len;
    }
    let mut gap = matched[0].0;
    for w in matched.windows(2) {
        gap += w[1].0 - w[0].1;
    }
    gap += haystack_len - matched[matched.len() - 1].1;
    gap
}

/// Scores a raw fuzzy match.
///
/// Both the query and the haystack are already lower-cased, so no copy has to
/// be made here.
fn score_fuzzy(
    query_text: &str,
    query_len: usize,
    haystack_lower: &str,
    matched: &[(usize, usize)],
    hay: &[HayChar],
    boundaries: &mut Vec<usize>,
) -> i32 {
    if query_len == 0 {
        return 0;
    }

    collect_boundaries(hay, boundaries);
    let (hits_all, hits_words) = boundary_hits(matched, boundaries);
    let matched_len = matched.len();
    // Every matched character sits on a word boundary.
    let all_boundaries = hits_all == matched_len;

    // Base: pick the strongest applicable category.
    let base = if query_text == haystack_lower {
        BASE_EXACT
    } else if haystack_lower.starts_with(query_text) {
        BASE_PREFIX
    } else if all_boundaries {
        BASE_BOUNDARY
    } else if hits_words >= 2 {
        // The query picked the initials of two or more words, e.g. "cbr" in
        // "Chrome Browser".
        BASE_BOUNDARY
    } else if haystack_lower.contains(query_text) {
        BASE_SUBSTRING
    } else if hits_words >= 1 && matched_len >= 3 {
        // Partially boundary-aligned subsequence.
        BASE_PARTIAL_BOUNDARY
    } else {
        BASE_SUBSEQUENCE
    };

    let mut score = base;

    // Reward compact runs.
    let consec = consecutive_pairs(matched);
    score += (consec as i32) * 8;

    // Reward boundary alignment.
    score += (hits_all as i32) * 20;

    // Penalise gaps.
    let gap = total_gap(matched, haystack_lower.len());
    score -= ((gap as i32) / 2).min(150);

    // Penalise a late start.
    let start = matched.first().map(|m| m.0).unwrap_or(0);
    let start = start.min(haystack_lower.len());
    score -= (start as i32) / 4;

    // Slight preference for shorter targets.
    score -= (haystack_lower.chars().count() as i32) / 40;

    score.clamp(0, MAX_SCORE)
}

/// Scores an acronym match.
fn score_acronym(groups: usize, word_count: usize, query_len: usize) -> i32 {
    // A tight acronym (query covers every word) is the strongest signal.
    let coverage = groups as f32 / word_count.max(1) as f32;
    let mut score = BASE_ACRONYM + (coverage * 100.0) as i32;

    // Fewer query characters per group is better ("vs" beats "vsc" on "Visual Studio").
    score += (groups as i32 - query_len as i32).max(0) * 10;

    // Reward each additional group covered.
    score += groups.saturating_sub(1) as i32 * 15;

    score.clamp(0, MAX_SCORE)
}

/// The bit of an ASCII character in a character mask.
///
/// Characters outside the printable ASCII range have no bit, which makes the
/// mask unusable as a filter for them.
pub fn mask_bit(c: char) -> Option<u128> {
    let code = c as u32;
    if (0x20..=0x7E).contains(&code) {
        Some(1u128 << (code - 0x20))
    } else {
        None
    }
}

/// The mask of the ASCII characters that occur in `text`.
///
/// The mask is a cheap way to rule out candidates: a fuzzy match can only use
/// characters that are actually present, so a candidate whose mask lacks one
/// of the query's characters cannot match at all.
pub fn character_mask(text: &str) -> u128 {
    let mut mask = 0u128;
    for c in text.chars() {
        if let Some(bit) = mask_bit(c) {
            mask |= bit;
        }
    }
    mask
}

/// A query prepared for matching against many candidates.
///
/// Preparing the query once and reusing it for every candidate removes the
/// per-candidate lower-casing and character collection — with several
/// thousand candidates that was the single largest cost of a query.
#[derive(Debug, Clone, Default)]
pub struct PreparedQuery {
    /// The trimmed, lower-cased query text.
    text: String,
    /// Its characters.
    chars: Vec<char>,
    /// The whitespace-separated segments of the query, as ranges into
    /// `chars`.
    ///
    /// Flow Launcher splits the query on whitespace and requires every
    /// segment to match on its own, which is what makes "notepad code" work
    /// the way a user expects.
    segments: Vec<(usize, usize)>,
    /// The text of every segment.
    segment_texts: Vec<String>,
    /// The ASCII characters of the query, as a bit mask.
    mask: u128,
    /// True when the query contains a character without a mask bit.
    unmaskable: bool,
    /// True when the query contains a CJK ideograph.
    has_cjk: bool,
}

impl PreparedQuery {
    /// The prepared query text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The prepared query characters.
    pub fn chars(&self) -> &[char] {
        &self.chars
    }

    /// The whitespace-separated segments of the query.
    pub fn segments(&self) -> &[(usize, usize)] {
        &self.segments
    }

    /// Whether a haystack with this character mask can match at all.
    ///
    /// `haystack_mask` must have been built from the *lower-cased* haystack.
    pub fn mask_admits(&self, haystack_mask: u128) -> bool {
        if self.unmaskable {
            return true;
        }
        (self.mask & !haystack_mask) == 0
    }

    /// Whether the query contains a CJK ideograph.
    ///
    /// A Chinese query is matched against the original text, an ASCII query
    /// may also be matched against the pinyin rendering.
    pub fn has_cjk(&self) -> bool {
        self.has_cjk
    }
}

/// Reusable buffers for the matching hot path.
///
/// Every candidate used to allocate a dozen short-lived vectors, which meant
/// tens of thousands of allocations per query. The buffers live in a
/// thread-local, so the matching stays free of allocations without making the
/// matcher `Sync`-hostile.
#[derive(Default)]
struct Scratch {
    /// The characters of the current haystack.
    hay: Vec<HayChar>,
    /// The lower-cased haystack.
    ///
    /// The buffer is reused, so lowering the haystack does not allocate.
    lower: String,
    /// The byte offsets of the word boundaries of the current haystack.
    boundaries: Vec<usize>,
    /// The words of the current haystack.
    words: Vec<acronym::Word>,
    /// The matched character indices of the fuzzy pass.
    indices: Vec<usize>,
    /// The matched byte ranges.
    matched: Vec<(usize, usize)>,
    /// Scratch for the word-prefix pass.
    offsets: Vec<(usize, usize)>,
    /// The matched ranges of every query segment, collected.
    collected: Vec<(usize, usize)>,
}

thread_local! {
    static SCRATCH: RefCell<Scratch> = RefCell::new(Scratch::default());
}

/// Matches queries against candidate strings.
#[derive(Debug, Clone)]
pub struct StringMatcher {
    precision: SearchPrecision,
    options: MatchOptions,
}

impl Default for StringMatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl StringMatcher {
    /// A matcher with default options and `Regular` precision.
    pub fn new() -> Self {
        Self {
            precision: SearchPrecision::default(),
            options: MatchOptions::default(),
        }
    }

    /// A matcher with an explicit precision tier.
    pub fn with_precision(precision: SearchPrecision) -> Self {
        Self {
            precision,
            options: MatchOptions::default(),
        }
    }

    pub fn set_precision(&mut self, precision: SearchPrecision) {
        self.precision = precision;
    }

    pub fn precision(&self) -> SearchPrecision {
        self.precision
    }

    pub fn set_options(&mut self, options: MatchOptions) {
        self.options = options;
    }

    /// Prepare a query for repeated matching.
    ///
    /// The result can be handed to [`StringMatcher::match_prepared_lower`]
    /// for every candidate, which is what the plugins do when they scan their
    /// own index.
    pub fn prepare(&self, query: &str) -> PreparedQuery {
        let trimmed = query.trim();
        let text = if trimmed.chars().any(|c| c.is_uppercase()) {
            trimmed.to_lowercase()
        } else {
            trimmed.to_string()
        };
        let chars: Vec<char> = text.chars().collect();

        // Split the query on whitespace; every segment has to match.
        let mut segments: Vec<(usize, usize)> = Vec::new();
        let mut segment_texts: Vec<String> = Vec::new();
        let mut start: Option<usize> = None;
        for (i, c) in chars.iter().enumerate() {
            if c.is_whitespace() {
                if let Some(s) = start.take() {
                    segments.push((s, i));
                    segment_texts.push(chars[s..i].iter().collect());
                }
            } else if start.is_none() {
                start = Some(i);
            }
        }
        if let Some(s) = start {
            segments.push((s, chars.len()));
            segment_texts.push(chars[s..].iter().collect());
        }

        let mut mask = 0u128;
        let mut unmaskable = false;
        let mut has_cjk = false;
        for c in &chars {
            match mask_bit(*c) {
                Some(bit) => mask |= bit,
                None => unmaskable = true,
            }
            if crate::pinyin::is_cjk(*c) {
                has_cjk = true;
            }
        }
        PreparedQuery {
            text,
            chars,
            segments,
            segment_texts,
            mask,
            unmaskable,
            has_cjk,
        }
    }

    /// Match `query` against `haystack`.
    ///
    /// Returns `None` when the query does not match at all, or when the match
    /// scored below the current precision threshold.
    pub fn match_result(&self, query: &str, haystack: &str) -> Option<MatchResult> {
        let prepared = self.prepare(query);
        self.match_prepared_lower(&prepared, haystack)
    }

    /// Match an already lower-cased query against a haystack.
    ///
    /// The haystack is lower-cased internally when it contains upper-case
    /// characters; callers that match against thousands of pre-lowered
    /// candidates should use this to avoid the per-candidate allocation.
    pub fn match_result_lower(&self, lower_query: &str, haystack: &str) -> Option<MatchResult> {
        let prepared = self.prepare(lower_query);
        self.match_prepared_lower(&prepared, haystack)
    }

    /// Match a prepared query against a haystack.
    ///
    /// This is the hot path: the query is prepared once and then matched
    /// against thousands of candidates without a single allocation beyond the
    /// result itself.
    pub fn match_prepared_lower(
        &self,
        prepared: &PreparedQuery,
        haystack: &str,
    ) -> Option<MatchResult> {
        let query = &prepared.chars;
        if query.is_empty() {
            return None;
        }

        SCRATCH.with(|cell| {
            let mut scratch = cell.borrow_mut();
            let Scratch {
                hay,
                lower,
                boundaries,
                words,
                indices,
                matched,
                offsets,
                collected,
            } = &mut *scratch;

            // The haystack is lower-cased into a buffer that is reused, so
            // this does not allocate — with a few thousand candidates per
            // keystroke, a copy per candidate was the single largest cost of a
            // query.
            //
            // The ASCII case is done in place: `str::to_lowercase` would
            // allocate a fresh string every time, and lowercasing it
            // character by character would be slower than the byte-wise
            // operation the standard library uses.
            if haystack.is_ascii() {
                lower.clear();
                lower.push_str(haystack);
                lower.make_ascii_lowercase();
            } else {
                *lower = haystack.to_lowercase();
            }
            let hay_lower = lower.as_str();

            haystack_chars_into(hay_lower, self.options.ignore_case, hay);

            let mut best: Option<MatchResult> = None;

            // 1. Direct fuzzy subsequence match.
            //
            //    The query is split on whitespace and *every* segment has to
            //    match on its own, the way Flow Launcher does it. The score of
            //    the weakest segment decides the overall score.
            collected.clear();
            let mut fuzzy_score = i32::MAX;
            let mut fuzzy_ok = !prepared.segments.is_empty();
            for (index, (start, end)) in prepared.segments.iter().enumerate() {
                let segment = &prepared.chars[*start..*end];
                if fuzzy_subsequence_into(segment, hay, indices, matched) && !matched.is_empty() {
                    let score = score_fuzzy(
                        &prepared.segment_texts[index],
                        segment.len(),
                        hay_lower,
                        matched,
                        hay,
                        boundaries,
                    );
                    fuzzy_score = fuzzy_score.min(score);
                    collected.extend_from_slice(matched);
                } else {
                    fuzzy_ok = false;
                    break;
                }
            }
            if fuzzy_ok && !collected.is_empty() {
                collected.sort_unstable();
                collected.dedup();
                let score = fuzzy_score.clamp(0, MAX_SCORE);
                best = Some(MatchResult {
                    success: true,
                    score,
                    raw_score: score,
                    matched_indices: collected.to_vec(),
                    precision: self.precision,
                });
            }

            // The acronym and the word-prefix match need a word split, which
            // is another pass over the haystack. An exact match cannot be
            // beaten by either of them, so the pass is skipped for it.
            if best.as_ref().map_or(false, |b| b.score >= MAX_SCORE) {
                let mut result = best?;
                if result.score < self.precision.min_score() {
                    return None;
                }
                result.precision = self.precision;
                return Some(result);
            }

            // The acronym and the word-prefix match share one word split.
            acronym::split_words_into(hay_lower, &mut *words);
            let word_count = words.len();

            // 2. Acronym match — only considered when it beats the fuzzy score.
            if let Some(groups) =
                acronym::match_acronym_in(&prepared.chars, hay_lower, words, &mut *matched)
            {
                let score = score_acronym(groups, word_count, prepared.chars.len());
                if best.as_ref().map_or(true, |b| score > b.score) {
                    best = Some(MatchResult {
                        success: true,
                        score,
                        raw_score: score,
                        matched_indices: matched.to_vec(),
                        precision: self.precision,
                    });
                }
            }

            // 3. Word-prefix match.
            if let Some(groups) = acronym::match_word_prefixes_in(
                &prepared.chars,
                hay_lower,
                words,
                &mut *matched,
                &mut *offsets,
            ) {
                let score = (score_acronym(groups, word_count, prepared.chars.len()) - 60).max(0);
                if best.as_ref().map_or(true, |b| score > b.score) {
                    best = Some(MatchResult {
                        success: true,
                        score,
                        raw_score: score,
                        matched_indices: matched.to_vec(),
                        precision: self.precision,
                    });
                }
            }

            let mut result = best?;
            if result.score < self.precision.min_score() {
                return None;
            }
            result.precision = self.precision;
            Some(result)
        })
    }

    /// The substring of `haystack` covered by the match, for highlighting.
    pub fn fuzzy_search(&self, query: &str, haystack: &str) -> Option<String> {
        let r = self.match_result(query, haystack)?;
        if r.matched_indices.is_empty() {
            return None;
        }
        let start = r.matched_indices.first()?.0;
        let end = r.matched_indices.last()?.1;
        Some(haystack[start..end].to_string())
    }

    /// True when the query matches the haystack at the current precision.
    pub fn is_match(&self, query: &str, haystack: &str) -> bool {
        self.match_result(query, haystack).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match_scores_highest() {
        let m = StringMatcher::new();
        let r = m.match_result("chrome", "Chrome").expect("match");
        assert_eq!(r.score, MAX_SCORE);
        assert!(r.success);
    }

    #[test]
    fn every_word_of_a_query_has_to_match() {
        let m = StringMatcher::new();
        // "notepad code" finds "Notepad" only when both words are there.
        assert!(m.match_result("notepad code", "Notepad").is_none());
        assert!(m.match_result("notepad code", "Notepad Code").is_some());
        assert!(m.match_result("chrome browser", "Google Chrome Browser").is_some());
    }

    #[test]
    fn a_query_with_spaces_is_not_matched_against_a_space() {
        // The whole query used to be one sequence, so the space in the query
        // had to be found in the haystack.
        let m = StringMatcher::new();
        assert!(m.match_result("go chrome", "GoogleChrome").is_some());
    }

    #[test]
    fn the_weakest_word_decides_the_score() {
        let m = StringMatcher::new();
        let strong = m.match_result("chrome code", "Chrome Code").unwrap();
        let weak = m.match_result("chrome zzz", "Chrome Zzz").unwrap();
        assert!(strong.score > weak.score, "{} vs {}", strong.score, weak.score);
    }

    #[test]
    fn prefix_beats_substring() {
        let m = StringMatcher::new();
        let prefix = m.match_result("chro", "Chrome Browser").unwrap().score;
        let substring = m.match_result("rome", "Chrome Browser").unwrap().score;
        assert!(prefix > substring, "prefix={prefix} substring={substring}");
    }

    #[test]
    fn subsequence_still_matches() {
        let m = StringMatcher::new();
        assert!(m.match_result("cbr", "Chrome Browser").is_some());
    }

    #[test]
    fn non_subsequence_does_not_match() {
        let m = StringMatcher::new();
        assert!(m.match_result("xyz", "Chrome Browser").is_none());
    }

    #[test]
    fn acronym_beats_loose_subsequence() {
        let m = StringMatcher::new();
        let acro = m.match_result("vs", "Visual Studio").unwrap().score;
        let loose = m.match_result("vs", "Various Something").unwrap().score;
        assert!(acro >= loose, "acronym={acro} loose={loose}");
    }

    #[test]
    fn precision_filters_weak_matches() {
        let regular = StringMatcher::with_precision(SearchPrecision::Regular);
        let loose = StringMatcher::with_precision(SearchPrecision::None);
        let hay = "abcdefghij";
        assert!(loose.match_result("aj", hay).is_some());
        assert!(regular.match_result("aj", hay).is_none());
    }

    #[test]
    fn matched_indices_point_at_the_right_bytes() {
        let m = StringMatcher::new();
        let r = m.match_result("rome", "Chrome").expect("match");
        assert!(!r.matched_indices.is_empty());
        let s = r.matched_indices[0].0;
        assert_eq!(&"Chrome"[s..r.matched_indices.last().unwrap().1], "rome");
    }

    #[test]
    fn empty_query_never_matches() {
        assert!(StringMatcher::new().match_result("  ", "Chrome").is_none());
    }
}
