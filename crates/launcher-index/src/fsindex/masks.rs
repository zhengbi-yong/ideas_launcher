use super::*;

/// The filter bit of a character.
///
/// The letters and the digits get a bucket each, so that the filter is as
/// sharp as it can be: a query for `report` then really asks for the letters
/// r, e, p, o and t, and not for the digits that happen to sit in the same
/// bucket. The other characters share the buckets that are left.
///
/// The bucketing makes the filter weaker than an exact set of characters,
/// which is fine: a filter may only rule out candidates that cannot match,
/// never the other way round.

pub(super) fn mask_bit(c: char) -> u64 {
    // Lower case the same way the matcher does, so that the mask of a query
    // and the mask of a name agree.
    let lower = if c.is_ascii() {
        c.to_ascii_lowercase()
    } else {
        c.to_lowercase().next().unwrap_or(c)
    };
    if lower.is_ascii() {
        return 1u64 << ascii_bit(lower as u8);
    }
    // The rest is spread over the buckets that are left.
    1u64 << (37 + (lower as u32).wrapping_sub(128) % 27)
}

/// The bucket of an ASCII character.
///
/// The letters and the digits get one each; the punctuation shares the
/// buckets that are left.
pub(super) fn ascii_bit(byte: u8) -> u32 {
    let lower = byte.to_ascii_lowercase();
    match lower {
        b'a'..=b'z' => (lower - b'a') as u32,
        b'0'..=b'9' => 26 + (lower - b'0') as u32,
        _ => 37 + (lower as u32).wrapping_sub(128) % 27,
    }
}

/// The filter mask of a whole name.
///
/// The names on a real volume are almost always plain ASCII, so the bytes are
/// walked directly and the UTF-8 decoder is only used for the few names that
/// are not.
pub(super) fn name_mask(name: &str) -> u64 {
    let bytes = name.as_bytes();
    let mut mask = 0u64;
    let mut index = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte < 0x80 {
            mask |= 1u64 << ascii_bit(byte);
            index += 1;
            continue;
        }
        let Some(c) = name[index..].chars().next() else {
            break;
        };
        mask |= mask_bit(c);
        index += c.len_utf8();
    }
    mask
}

/// The characters a name has to contain for the query to match.
///
/// `None` means the query does not constrain the name, so no filter may be
/// used.
pub(super) fn required_mask(node: &Node, wildcards: bool) -> Option<u64> {
    match node {
        Node::Term(term) => Some(term_mask(term, wildcards)),
        Node::And(children) => {
            let mut mask = 0u64;
            for child in children {
                if let Some(child_mask) = required_mask(child, wildcards) {
                    mask |= child_mask;
                }
            }
            if mask == 0 { None } else { Some(mask) }
        }
        // An `or` can be satisfied by either branch, which no single mask can
        // express, and a `not` says nothing about the name.
        _ => None,
    }
}

/// The characters a term requires of a name.
///
/// Wildcards are left out: `*.pdf` only requires the characters of `.pdf`.
pub(super) fn term_mask(term: &Term, wildcards: bool) -> u64 {
    let mut mask = 0u64;
    for c in term.lower.chars() {
        if wildcards && (c == '*' || c == '?') {
            continue;
        }
        mask |= mask_bit(c);
    }
    mask
}
