//! A small regular expression engine.
//!
//! Everything's `regex:` modifier needs one, and the launcher has no regex
//! dependency of its own — the whole point of the project is to stay small.
//! So here is an engine that covers what a file name search needs:
//!
//! | syntax | meaning |
//! |---|---|
//! | `abc` | the letters |
//! | `.` | any one character |
//! | `[abc]` `[a-z]` `[^abc]` | a set of characters |
//! | `*` `+` `?` | zero or more, one or more, zero or one |
//! | `{n}` `{n,m}` | a number of repetitions |
//! | `\|` | one of the alternatives |
//! | `(...)` | a group |
//! | `^` `$` | the start and the end of the name |
//!
//! The matcher walks the pattern with backtracking, which is fast enough for
//! the short patterns a file name search uses.

/// A compiled regular expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Regex {
    /// The alternatives of the whole pattern; each one is a sequence of
    /// pieces.
    alternatives: Vec<Vec<Node>>,
}

/// One piece of a pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    /// One letter.
    Literal(char),
    /// Any one character.
    Any,
    /// A set of characters.
    Class {
        /// True when the set is inverted.
        negated: bool,
        /// The ranges of the set.
        ranges: Vec<(char, char)>,
    },
    /// The start of the name.
    Start,
    /// The end of the name.
    End,
    /// A group with alternatives.
    Group(Vec<Vec<Node>>),
    /// A piece that is repeated.
    Repeat {
        node: Box<Node>,
        min: u32,
        max: u32,
    },
}

impl Regex {
    /// Compile a pattern.
    ///
    /// Returns `None` when the pattern does not make sense.
    pub fn new(pattern: &str) -> Option<Self> {
        let chars: Vec<char> = pattern.chars().collect();
        let mut parser = Parser { chars, at: 0 };
        let alternatives = parser.parse_alternatives()?;
        if parser.at != parser.chars.len() {
            return None;
        }
        Some(Self { alternatives })
    }

    /// Whether the text matches the pattern.
    ///
    /// The pattern is looked for anywhere in the text, the way a regular
    /// expression is normally used. The anchors `^` and `$` still refer to
    /// the start and the end of the whole text.
    pub fn is_match(&self, text: &str) -> bool {
        let chars: Vec<char> = text.chars().collect();
        for start in 0..=chars.len() {
            for alternative in &self.alternatives {
                if match_nodes_sequence(alternative, &chars, start, &mut |_| true) {
                    return true;
                }
            }
        }
        false
    }
}

/// The parser of a pattern.
struct Parser {
    chars: Vec<char>,
    at: usize,
}

impl Parser {
    /// Parse the alternatives of a group.
    fn parse_alternatives(&mut self) -> Option<Vec<Vec<Node>>> {
        let mut alternatives = Vec::new();
        let mut current = Vec::new();
        loop {
            match self.chars.get(self.at).copied() {
                None => {
                    alternatives.push(current);
                    return Some(alternatives);
                }
                Some(')') => {
                    alternatives.push(current);
                    return Some(alternatives);
                }
                Some('|') => {
                    self.at += 1;
                    alternatives.push(std::mem::take(&mut current));
                }
                Some(_) => {
                    let node = self.parse_piece()?;
                    current.push(node);
                }
            }
        }
    }

    /// Parse one piece of a pattern and the repetitions that follow it.
    fn parse_piece(&mut self) -> Option<Node> {
        let node = match self.chars.get(self.at).copied()? {
            '(' => {
                self.at += 1;
                let alternatives = self.parse_alternatives()?;
                if self.chars.get(self.at).copied() != Some(')') {
                    return None;
                }
                self.at += 1;
                Node::Group(alternatives)
            }
            '[' => {
                self.at += 1;
                let (negated, ranges) = self.parse_class()?;
                Node::Class { negated, ranges }
            }
            '.' => {
                self.at += 1;
                Node::Any
            }
            '^' => {
                self.at += 1;
                Node::Start
            }
            '$' => {
                self.at += 1;
                Node::End
            }
            '\\' => {
                self.at += 1;
                let escaped = self.chars.get(self.at).copied()?;
                self.at += 1;
                match escaped {
                    'd' => Node::Class {
                        negated: false,
                        ranges: vec![('0', '9')],
                    },
                    'w' => Node::Class {
                        negated: false,
                        ranges: vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')],
                    },
                    's' => Node::Class {
                        negated: false,
                        ranges: vec![(' ', ' '), ('\t', '\t'), ('\n', '\n'), ('\r', '\r')],
                    },
                    other => Node::Literal(other),
                }
            }
            ')' | '|' => return None,
            other => {
                self.at += 1;
                Node::Literal(other)
            }
        };
        // The repetitions that follow.
        let mut node = node;
        loop {
            match self.chars.get(self.at).copied() {
                Some('*') => {
                    self.at += 1;
                    node = Node::Repeat {
                        node: Box::new(node),
                        min: 0,
                        max: u32::MAX,
                    };
                }
                Some('+') => {
                    self.at += 1;
                    node = Node::Repeat {
                        node: Box::new(node),
                        min: 1,
                        max: u32::MAX,
                    };
                }
                Some('?') => {
                    self.at += 1;
                    node = Node::Repeat {
                        node: Box::new(node),
                        min: 0,
                        max: 1,
                    };
                }
                Some('{') => {
                    let Some((min, max, len)) = self.parse_count() else {
                        break;
                    };
                    self.at += len;
                    node = Node::Repeat {
                        node: Box::new(node),
                        min,
                        max,
                    };
                }
                _ => break,
            }
        }
        Some(node)
    }

    /// Parse the character set of a class.
    fn parse_class(&mut self) -> Option<(bool, Vec<(char, char)>)> {
        let mut negated = false;
        if self.chars.get(self.at).copied() == Some('^') {
            negated = true;
            self.at += 1;
        }
        let mut ranges = Vec::new();
        let mut first = true;
        loop {
            let c = self.chars.get(self.at).copied()?;
            if c == ']' && !first {
                self.at += 1;
                return Some((negated, ranges));
            }
            first = false;
            self.at += 1;
            let start = if c == '\\' {
                let escaped = self.chars.get(self.at).copied()?;
                self.at += 1;
                match escaped {
                    'd' => {
                        ranges.push(('0', '9'));
                        continue;
                    }
                    'w' => {
                        ranges.push(('a', 'z'));
                        ranges.push(('A', 'Z'));
                        ranges.push(('0', '9'));
                        ranges.push(('_', '_'));
                        continue;
                    }
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    other => other,
                }
            } else {
                c
            };
            // A range, when the next character is a dash.
            if self.chars.get(self.at).copied() == Some('-')
                && self.chars.get(self.at + 1).copied().is_some_and(|c| c != ']')
            {
                self.at += 1;
                let end = self.chars.get(self.at).copied()?;
                self.at += 1;
                ranges.push((start, end));
            } else {
                ranges.push((start, start));
            }
        }
    }

    /// Parse the `{n}` or `{n,m}` of a repetition.
    ///
    /// Returns the bounds and how many characters were used.
    fn parse_count(&self) -> Option<(u32, u32, usize)> {
        let mut at = self.at + 1;
        let mut min = String::new();
        while let Some(c) = self.chars.get(at).copied() {
            if c.is_ascii_digit() {
                min.push(c);
                at += 1;
            } else {
                break;
            }
        }
        if min.is_empty() {
            return None;
        }
        let min_value: u32 = min.parse().ok()?;
        match self.chars.get(at).copied() {
            Some('}') => Some((min_value, min_value, at + 1 - self.at)),
            Some(',') => {
                at += 1;
                let mut max = String::new();
                while let Some(c) = self.chars.get(at).copied() {
                    if c.is_ascii_digit() {
                        max.push(c);
                        at += 1;
                    } else {
                        break;
                    }
                }
                if self.chars.get(at).copied() != Some('}') {
                    return None;
                }
                let max_value = if max.is_empty() {
                    u32::MAX
                } else {
                    max.parse().ok()?
                };
                Some((min_value, max_value, at + 1 - self.at))
            }
            _ => None,
        }
    }
}

/// Try to match a piece of the pattern at a place in the text.
///
/// `rest` is called with the position after the piece; when it returns true
/// the whole pattern matched.
fn match_nodes(
    node: &Node,
    text: &[char],
    at: usize,
    rest: &mut dyn FnMut(usize) -> bool,
) -> bool {
    match node {
        Node::Literal(c) => {
            if text.get(at) == Some(c) {
                rest(at + 1)
            } else {
                false
            }
        }
        Node::Any => {
            if at < text.len() {
                rest(at + 1)
            } else {
                false
            }
        }
        Node::Class { negated, ranges } => {
            let Some(c) = text.get(at) else {
                return false;
            };
            let inside = ranges.iter().any(|(low, high)| c >= low && c <= high);
            if inside != *negated {
                rest(at + 1)
            } else {
                false
            }
        }
        Node::Start => {
            if at == 0 {
                rest(at)
            } else {
                false
            }
        }
        Node::End => {
            if at == text.len() {
                rest(at)
            } else {
                false
            }
        }
        Node::Group(alternatives) => {
            for alternative in alternatives {
                let mut matched = false;
                let mut try_rest = |position: usize| -> bool {
                    matched = true;
                    rest(position)
                };
                if match_nodes_sequence(alternative, text, at, &mut try_rest) {
                    return true;
                }
                let _ = matched;
            }
            false
        }
        Node::Repeat { node, min, max } => {
            match_repeat(node, *min, *max, 0, text, at, rest)
        }
    }
}

/// Try to match a sequence of pieces.
fn match_nodes_sequence(
    nodes: &[Node],
    text: &[char],
    at: usize,
    rest: &mut dyn FnMut(usize) -> bool,
) -> bool {
    match nodes.split_first() {
        None => rest(at),
        Some((first, tail)) => {
            let mut continuation = |position: usize| match_nodes_sequence(tail, text, position, rest);
            match_nodes(first, text, at, &mut continuation)
        }
    }
}

/// Try to match a piece that is repeated.
///
/// `count` is how many times the piece has been matched so far, so that the
/// matcher knows when it has to stop.
fn match_repeat(
    node: &Node,
    min: u32,
    max: u32,
    count: u32,
    text: &[char],
    at: usize,
    rest: &mut dyn FnMut(usize) -> bool,
) -> bool {
    // The greedy way: as many as possible first.
    if count < max {
        let mut continuation = |position: usize| -> bool {
            if position == at {
                // Nothing was consumed; stop, or it would loop for ever.
                return false;
            }
            match_repeat(node, min, max, count + 1, text, position, rest)
        };
        if match_nodes(node, text, at, &mut continuation) {
            return true;
        }
    }
    if count >= min {
        return rest(at);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pattern: &str, text: &str) -> bool {
        Regex::new(pattern)
            .unwrap_or_else(|| panic!("the pattern {pattern:?} should compile"))
            .is_match(text)
    }

    #[test]
    fn plain_letters_match() {
        assert!(matches("report", "report.pdf"));
        assert!(matches("report", "the report.pdf"));
        assert!(!matches("report", "summary.txt"));
    }

    #[test]
    fn the_anchors_work() {
        assert!(matches("^report", "report.pdf"));
        assert!(!matches("^report", "the report.pdf"));
        assert!(matches("pdf$", "the report.pdf"));
        assert!(!matches("pdf$", "the report.pdfx"));
    }

    #[test]
    fn the_dot_matches_one_character() {
        assert!(matches("r.port", "report.pdf"));
        assert!(!matches("r..port", "report.pdf"));
    }

    #[test]
    fn the_quantifiers_work() {
        assert!(matches("ab*c", "ac"));
        assert!(matches("ab*c", "abbbc"));
        assert!(matches("ab+c", "abbbc"));
        assert!(!matches("ab+c", "ac"));
        assert!(matches("ab?c", "ac"));
        assert!(!matches("ab?c", "abbc"));
    }

    #[test]
    fn the_counts_work() {
        assert!(matches("a{3}", "aaa"));
        assert!(!matches("a{3}", "aa"));
        assert!(matches("a{2,3}", "aa"));
        assert!(matches("a{2,3}", "aaa"));
        assert!(!matches("a{2,3}", "a"));
    }

    #[test]
    fn the_classes_work() {
        assert!(matches("[0-9]+", "file42"));
        assert!(!matches("^[0-9]+$", "file42"));
        assert!(matches("[^0-9]+", "file"));
        assert!(matches("file\\.\\w+", "file.txt"));
    }

    #[test]
    fn the_alternatives_work() {
        assert!(matches("cat|dog", "dog.txt"));
        assert!(matches("(cat|dog)s", "dogs.txt"));
        assert!(!matches("^(cat|dog)s$", "dogs.txt"));
    }

    #[test]
    fn a_broken_pattern_is_rejected() {
        assert!(Regex::new("(unclosed").is_none());
        assert!(Regex::new("[unclosed").is_none());
        assert!(Regex::new("a)").is_none());
    }
}
