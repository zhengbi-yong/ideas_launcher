//! Everything-style search syntax.
//!
//! Everything (voidtools) accepts a small query language in its search box:
//! boolean operators, modifiers that switch matching modes, and `function:`
//! filters over indexed metadata. The grammar implemented here follows the
//! official description shipped with Everything 1.4.1 (`Everything\searching`),
//! which was read out of the offline help archive of the signed release.
//!
//! ```text
//! search  := or
//! or      := and ( '|' and )*
//! and     := unary ( <implicit AND> unary )*
//! unary   := '!' unary | '<' or '>' | term
//! term    := [ 'name:' ] [ '"' value '"' ]
//! ```
//!
//! The parser is deliberately independent of any index: it produces an AST
//! that is evaluated against a [`SearchTarget`], so it can be unit tested
//! without touching the file system.

/// A comparison used by `function:<op>value`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareOp {
    /// `function:value`
    Eq,
    /// `function:<value`
    Lt,
    /// `function:<=value`
    Le,
    /// `function:>value`
    Gt,
    /// `function:>=value`
    Ge,
}

impl CompareOp {
    /// Read a leading comparison operator off `raw`.
    fn parse(raw: &str) -> (CompareOp, &str) {
        if let Some(rest) = raw.strip_prefix("<=") {
            return (CompareOp::Le, rest);
        }
        if let Some(rest) = raw.strip_prefix(">=") {
            return (CompareOp::Ge, rest);
        }
        if let Some(rest) = raw.strip_prefix("<") {
            return (CompareOp::Lt, rest);
        }
        if let Some(rest) = raw.strip_prefix(">") {
            return (CompareOp::Gt, rest);
        }
        if let Some(rest) = raw.strip_prefix("=") {
            return (CompareOp::Eq, rest);
        }
        (CompareOp::Eq, raw)
    }
}

/// A modifier that switches a matching mode on or off.
///
/// Modifiers with a value (`path:c:\temp`) additionally contribute a term.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    /// `file:` — only files.
    FilesOnly,
    /// `folder:` — only folders.
    FoldersOnly,
    /// `nofileonly:` — folders are allowed again.
    NoFilesOnly,
    /// `nofolderonly:` — files are allowed again.
    NoFoldersOnly,
    /// `case:` — case sensitive.
    Case,
    /// `nocase:` — case insensitive.
    NoCase,
    /// `regex:` — the remaining terms are regular expressions.
    Regex,
    /// `noregex:` — regular expressions are off.
    NoRegex,
    /// `wholeword:` / `ww:` — only match whole words.
    WholeWord,
    /// `nowholeword:` / `noww:` — match anywhere.
    NoWholeWord,
    /// `wildcards:` — `*` and `?` are active.
    Wildcards,
    /// `nowildcards:` — `*` and `?` are literal.
    NoWildcards,
    /// `path:` — match against the full path.
    MatchPath,
    /// `nopath:` — match against the file name only.
    NoMatchPath,
    /// `diacritics:` — accent marks matter.
    Diacritics,
    /// `nodiacritics:` — accent marks are ignored.
    NoDiacritics,
    /// `utf8:` — compare as UTF-8.
    Utf8,
    /// `noascii:` — compare as ASCII.
    NoAscii,
    /// `exact:` / `wfn:` / `wholefilename:` — the term must match the whole name.
    Exact,
    /// `nowfn:` / `nowholefilename:` — the term may match part of the name.
    NoExact,
}

/// A literal search term.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    /// The text exactly as the user typed it.
    pub text: String,
    /// The lower cased text.
    ///
    /// Kept next to the original so that matching a million entries does not
    /// allocate a lower cased copy of the term for every single one.
    pub lower: String,
    /// The compiled pattern, when the regex mode is on.
    ///
    /// It is filled in after the whole query has been parsed, because only
    /// then it is known whether the regex mode was asked for.
    pub regex: Option<crate::regex::Regex>,
}

impl Term {
    fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let lower = text.to_lowercase();
        Self {
            text,
            lower,
            regex: None,
        }
    }
}

/// A node of the parsed search tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// Matches everything. Produced by an empty query.
    All,
    /// Logical AND.
    And(Vec<Node>),
    /// Logical OR.
    Or(Vec<Node>),
    /// Logical NOT.
    Not(Box<Node>),
    /// A literal search term.
    Term(Term),
    /// A `function:value` filter.
    Function {
        /// The function name, lower case (`size`, `dm`, `ext`, …).
        name: String,
        /// The comparison operator.
        op: CompareOp,
        /// The value to compare against.
        value: String,
        /// The upper bound of a range, when the value was written as `a..b`.
        value2: Option<String>,
    },
    /// A mode switch.
    Flag(Flag),
}

/// The modifiers that only switch a mode on or off.
const FLAGS: &[(&str, Flag)] = &[
    ("file", Flag::FilesOnly),
    ("files", Flag::FilesOnly),
    ("nofileonly", Flag::NoFilesOnly),
    ("folder", Flag::FoldersOnly),
    ("folders", Flag::FoldersOnly),
    ("nofolderonly", Flag::NoFoldersOnly),
    ("case", Flag::Case),
    ("nocase", Flag::NoCase),
    ("regex", Flag::Regex),
    ("noregex", Flag::NoRegex),
    ("wholeword", Flag::WholeWord),
    ("ww", Flag::WholeWord),
    ("nowholeword", Flag::NoWholeWord),
    ("noww", Flag::NoWholeWord),
    ("wildcards", Flag::Wildcards),
    ("nowildcards", Flag::NoWildcards),
    ("path", Flag::MatchPath),
    ("nopath", Flag::NoMatchPath),
    ("diacritics", Flag::Diacritics),
    ("nodiacritics", Flag::NoDiacritics),
    ("utf8", Flag::Utf8),
    ("noascii", Flag::NoAscii),
    ("exact", Flag::Exact),
    ("wfn", Flag::Exact),
    ("wholefilename", Flag::Exact),
    ("nowfn", Flag::NoExact),
    ("nowholefilename", Flag::NoExact),
];

/// The `function:` filters understood by the evaluator.
///
/// `size`, `dm`, `dc`, `da` and `ext` are the ones that carry their weight in a
/// launcher; the rest are recognised so that a query is not silently turned
/// into a literal term.
const FUNCTIONS: &[&str] = &[
    "size", "dm", "datemodified", "dc", "datecreated", "da", "dateaccessed", "dr", "daterun", "rc",
    "recentchange", "ext", "len", "parent", "infolder", "nosubfolders", "depth", "parents", "attrib",
    "attributes", "count", "startwith", "endwith", "empty", "root", "child", "childcount",
    "childfilecount", "childfoldercount", "dupe", "sizedupe", "dadupe", "dcdupe", "dmdupe",
    "attribdupe", "namepartdupe", "fsi", "frn", "type", "shell", "content", "ansicontent",
    "utf8content", "utf16content", "utf16becontent", "album", "artist", "genre", "title", "track",
    "comment", "year", "width", "height", "dimension", "dimensions", "bitdepth", "orientation",
];

/// A macro expands into a fixed search.
///
/// The six file-type macros expand to an `ext:` filter. The lists were read
/// out of the signed Everything 1.4.1 release, where they are stored as
/// literals next to the search parser.
const MACROS: &[(&str, &str)] = &[
    ("quot", "\""),
    ("apos", "'"),
    ("amp", "&"),
    ("lt", "<"),
    ("gt", ">"),
    (
        "audio",
        "ext:aac;ac3;aif;aifc;aiff;au;cda;dts;fla;flac;it;m1a;m2a;m3u;m4a;m4b;m4p;mid;midi;mka;mod;mp2;mp3;mpa;ogg;ra;rmi;snd;spc;umx;voc;wav;wma;xm",
    ),
    (
        "zip",
        "ext:7z;ace;arj;bz2;cab;gz;gzip;jar;r00;r01;r02;r03;r04;r05;r06;r07;r08;r09;r10;r11;r12;r13;r14;r15;r16;r17;r18;r19;r20;r21;r22;r23;r24;r25;r26;r27;r28;r29;rar;tar;tgz;z;zip",
    ),
    (
        "doc",
        "ext:c;chm;cpp;csv;cxx;doc;docm;docx;dot;dotm;dotx;h;hpp;htm;html;hxx;ini;java;lua;mht;mhtml;odt;pdf;potx;potm;ppam;ppsm;ppsx;pps;ppt;pptm;pptx;rtf;sldm;sldx;thmx;txt;vsd;wpd;wps;wri;xlam;xls;xlsb;xlsm;xlsx;xltm;xltx;xml",
    ),
    ("exe", "ext:bat;cmd;exe;msi;msp;scr"),
    (
        "pic",
        "ext:ani;bmp;gif;ico;jpe;jpeg;jpg;pcx;png;psd;tga;tif;tiff;webp;wmf",
    ),
    (
        "video",
        "ext:3g2;3gp;3gp2;3gpp;amr;amv;asf;avi;bdmv;bik;d2v;divx;drc;dsa;dsm;dss;dsv;evo;f4v;flc;fli;flic;flv;hdmov;ifo;ivf;m1v;m2p;m2t;m2ts;m2v;m4v;mkv;mp2v;mp4;mp4v;mpe;mpeg;mpg;mpls;mpv2;mpv4;mov;mts;ogm;ogv;pss;pva;qt;ram;ratdvd;rm;rmm;rmvb;roq;rpm;smil;smk;swf;tp;tpr;ts;vob;vp6;webm;wm;wmp;wmv",
    ),
];

/// The resolved matching options for one query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchOptions {
    /// Case sensitive matching.
    pub case_sensitive: bool,
    /// Match against the full path instead of the file name.
    pub match_path: bool,
    /// Only files.
    pub files_only: bool,
    /// Only folders.
    pub folders_only: bool,
    /// The term must start at a word boundary.
    pub whole_word: bool,
    /// `*` and `?` are wildcards.
    pub wildcards: bool,
    /// The term must match the whole file name.
    pub whole_filename: bool,
    /// Accent marks are significant.
    pub diacritics: bool,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            case_sensitive: false,
            match_path: false,
            files_only: false,
            folders_only: false,
            whole_word: false,
            wildcards: true,
            whole_filename: true,
            diacritics: true,
        }
    }
}

impl SearchOptions {
    /// Apply one modifier to these options.
    pub fn apply(&mut self, flag: Flag) {
        match flag {
            Flag::FilesOnly => {
                self.files_only = true;
                self.folders_only = false;
            }
            Flag::FoldersOnly => {
                self.folders_only = true;
                self.files_only = false;
            }
            Flag::NoFilesOnly => self.files_only = false,
            Flag::NoFoldersOnly => self.folders_only = false,
            Flag::Case => self.case_sensitive = true,
            Flag::NoCase => self.case_sensitive = false,
            Flag::Regex => {}
            Flag::NoRegex => {}
            Flag::WholeWord => self.whole_word = true,
            Flag::NoWholeWord => self.whole_word = false,
            Flag::Wildcards => self.wildcards = true,
            Flag::NoWildcards => self.wildcards = false,
            Flag::MatchPath => self.match_path = true,
            Flag::NoMatchPath => self.match_path = false,
            Flag::Diacritics => self.diacritics = true,
            Flag::NoDiacritics => self.diacritics = false,
            Flag::Utf8 => {}
            Flag::NoAscii => {}
            Flag::Exact => self.whole_filename = true,
            Flag::NoExact => self.whole_filename = false,
        }
    }
}

/// The result of parsing a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSearch {
    /// The query tree.
    pub node: Node,
    /// The options implied by the modifiers in the query.
    pub options: SearchOptions,
    /// True when the query asks for regular expressions.
    pub regex: bool,
}

impl ParsedSearch {
    /// Parse `query`.
    pub fn parse(query: &str) -> Self {
        let mut parser = Parser::new(query);
        let mut node = parser.parse_or();
        let mut options = SearchOptions::default();
        let mut regex = false;
        collect_options(&mut node, &mut options, &mut regex);
        // The regular expressions are compiled once, after the whole query is
        // known, so that the mode does not have to be guessed while parsing.
        if regex {
            compile_regexes(&mut node);
        }
        // `wholefilename` only matters once wildcards are in play.
        if !options.wildcards {
            options.whole_filename = false;
        }
        ParsedSearch {
            node,
            options,
            regex,
        }
    }

    /// True when the query contains no term that can match anything.
    pub fn is_empty(&self) -> bool {
        matches!(self.node, Node::All)
    }
}

/// Compile the patterns of every term of a query.
fn compile_regexes(node: &mut Node) {
    match node {
        Node::Term(term) => {
            term.regex = crate::regex::Regex::new(&term.text);
        }
        Node::And(children) | Node::Or(children) => {
            for child in children {
                compile_regexes(child);
            }
        }
        Node::Not(inner) => compile_regexes(inner),
        _ => {}
    }
}

/// Walk the tree and fold every modifier into the options.
fn collect_options(node: &Node, options: &mut SearchOptions, regex: &mut bool) {
    match node {
        Node::Flag(flag) => {
            if *flag == Flag::Regex {
                *regex = true;
            }
            if *flag == Flag::NoRegex {
                *regex = false;
            }
            options.apply(*flag);
        }
        Node::And(children) | Node::Or(children) => {
            for child in children {
                collect_options(child, options, regex);
            }
        }
        Node::Not(inner) => collect_options(inner, options, regex),
        _ => {}
    }
}

/// The first plain term of a query.
///
/// Used for ranking: the term the user typed is what decides how well a
/// result matches, while the other nodes only narrow the set down.
pub fn primary_term(node: &Node) -> Option<&Term> {
    match node {
        Node::Term(term) => Some(term),
        Node::And(children) | Node::Or(children) => children.iter().find_map(primary_term),
        Node::Not(inner) => primary_term(inner),
        _ => None,
    }
}

/// Every plain term of a query.
///
/// The index uses them to work out which characters a name has to have for
/// a term to be able to match it.
pub fn terms(node: &Node) -> Vec<&Term> {
    let mut out = Vec::new();
    fn walk<'a>(node: &'a Node, out: &mut Vec<&'a Term>) {
        match node {
            Node::Term(term) => out.push(term),
            Node::And(children) | Node::Or(children) => {
                for child in children {
                    walk(child, out);
                }
            }
            Node::Not(inner) => walk(inner, out),
            _ => {}
        }
    }
    walk(node, &mut out);
    out
}

/// Whether a query needs the metadata of an entry.
///
/// The metadata is the size and the three timestamps. Fetching it costs a
/// file open, so it is only done when a filter really asks for it.
pub fn needs_metadata(node: &Node) -> bool {    match node {
        Node::Function { name, .. } => matches!(
            name.as_str(),
            "size" | "dm" | "datemodified" | "dc" | "datecreated" | "da" | "dateaccessed"
                | "dr" | "daterun" | "rc" | "recentchange"
        ),
        Node::And(children) | Node::Or(children) => children.iter().any(needs_metadata),
        Node::Not(inner) => needs_metadata(inner),
        _ => false,
    }
}

/// The values of every function with that name in a query.
///
/// Some functions cannot be answered by looking at one entry alone — the
/// duplicates and the children need the whole table — so the index asks for
/// their values and does the work itself.
pub fn function_values<'a>(node: &'a Node, name: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    fn walk<'a>(node: &'a Node, name: &str, out: &mut Vec<&'a str>) {
        match node {
            Node::Function {
                name: function,
                value,
                ..
            } if function == name => out.push(value),
            Node::And(children) | Node::Or(children) => {
                for child in children {
                    walk(child, name, out);
                }
            }
            Node::Not(inner) => walk(inner, name, out),
            _ => {}
        }
    }
    walk(node, name, &mut out);
    out
}

/// The entry a search is evaluated against.
pub trait SearchTarget {
    /// The file or folder name.
    fn name(&self) -> &str;
    /// The name of the containing folder.
    fn parent_name(&self) -> &str;
    /// The full path. Only called for path-dependent predicates.
    fn path(&self) -> String;
    /// An alternative spelling of the name.
    ///
    /// The launcher uses this for the pinyin rendering of a Chinese file
    /// name, so that `chongqi` finds `重启`.
    fn alias(&self) -> Option<&str> {
        None
    }
    /// True for folders.
    fn is_dir(&self) -> bool;
    /// Size in bytes.
    fn size(&self) -> u64;
    /// Creation time, seconds since the Unix epoch.
    fn created(&self) -> u64;
    /// Modification time, seconds since the Unix epoch.
    fn modified(&self) -> u64;
    /// Access time, seconds since the Unix epoch.
    fn accessed(&self) -> u64;
    /// File attributes.
    fn attributes(&self) -> u32;
    /// Folder depth, 0 for a volume root.
    fn depth(&self) -> u32;
}

/// Evaluate `node` against `target`.
pub fn evaluate(node: &Node, target: &dyn SearchTarget, options: &SearchOptions) -> bool {
    evaluate_scored(node, target, options).is_some()
}

/// The score of a name that matches the term exactly.
pub const SCORE_EXACT: i32 = 1000;
/// The score of a name that starts with the term.
pub const SCORE_PREFIX: i32 = 800;
/// The score of a name that holds the term somewhere.
pub const SCORE_CONTAINS: i32 = 600;
/// The score of a node that does not say anything about the name.
const NEUTRAL: i32 = i32::MAX / 2;

/// Evaluate `node` against `target` and return a score when it matches.
///
/// `None` means the target does not match. The score says how well it
/// matched: an exact name beats a prefix, and a prefix beats a match in the
/// middle. Folding the score into the match means the name is walked once
/// instead of twice, which matters when a million candidates are scanned.
pub fn evaluate_scored(
    node: &Node,
    target: &dyn SearchTarget,
    options: &SearchOptions,
) -> Option<i32> {
    match node {
        Node::All => Some(NEUTRAL),
        Node::And(children) => {
            let mut score = NEUTRAL;
            for child in children {
                let child_score = evaluate_scored(child, target, options)?;
                score = score.min(child_score);
            }
            Some(score)
        }
        Node::Or(children) => {
            let mut best: Option<i32> = None;
            for child in children {
                if let Some(child_score) = evaluate_scored(child, target, options) {
                    best = Some(best.map_or(child_score, |current: i32| current.max(child_score)));
                }
            }
            best
        }
        Node::Not(inner) => match evaluate_scored(inner, target, options) {
            Some(_) => None,
            None => Some(0),
        },
        Node::Flag(flag) => match flag {
            Flag::FilesOnly => (!target.is_dir()).then_some(NEUTRAL),
            Flag::FoldersOnly => target.is_dir().then_some(NEUTRAL),
            _ => Some(NEUTRAL),
        },
        Node::Term(term) => score_term(term, target, options),
        Node::Function {
            name,
            op,
            value,
            value2,
        } => {
            if match_function(name, *op, value, value2.as_deref(), target, options) {
                Some(NEUTRAL)
            } else {
                None
            }
        }
    }
}

/// Score a term against a target.
///
/// The name is tried first, then the pinyin rendering, then the name of the
/// folder that holds the entry.
fn score_term(term: &Term, target: &dyn SearchTarget, options: &SearchOptions) -> Option<i32> {
    if term.text.is_empty() {
        return Some(NEUTRAL);
    }
    if options.match_path {
        let path = target.path();
        if let Some(quality) = quality_of(term, &path, options) {
            return Some(quality);
        }
    }
    if let Some(quality) = quality_of(term, target.name(), options) {
        return Some(quality);
    }
    if let Some(alias) = target.alias() {
        if let Some(quality) = quality_of(term, alias, options) {
            return Some(quality - 100);
        }
    }
    let parent = target.parent_name();
    if !parent.is_empty() {
        if let Some(quality) = quality_of(term, parent, options) {
            return Some(quality - 300);
        }
    }
    None
}

/// How well a term matches one string.
///
/// One pass over the string answers all three cases: the position of the
/// match says whether it is an exact, a prefix or a match in the middle.
fn quality_of(term: &Term, haystack: &str, options: &SearchOptions) -> Option<i32> {
    // In the regex mode the pattern decides, and nothing else does.
    if let Some(pattern) = term.regex.as_ref() {
        return pattern
            .is_match(haystack)
            .then_some(SCORE_CONTAINS);
    }
    if options.wildcards && (term.text.contains('*') || term.text.contains('?')) {
        return if options.whole_filename {
            wildcard_match(&term.text, haystack, options.case_sensitive).then_some(SCORE_PREFIX)
        } else {
            wildcard_contains(&term.text, haystack, options.case_sensitive).then_some(SCORE_CONTAINS)
        };
    }
    if options.case_sensitive {
        if haystack == term.text {
            return Some(SCORE_EXACT);
        }
        if haystack.starts_with(&term.text) {
            return Some(SCORE_PREFIX);
        }
        if haystack.contains(&term.text) {
            return Some(SCORE_CONTAINS);
        }
        if options.whole_word && contains_whole_word(haystack, &term.text, true) {
            return Some(SCORE_CONTAINS - 100);
        }
        return None;
    }
    match find_fold(haystack, &term.lower) {
        Some(0) => {
            if haystack.eq_ignore_ascii_case(&term.text) {
                Some(SCORE_EXACT)
            } else {
                Some(SCORE_PREFIX)
            }
        }
        Some(_) => Some(SCORE_CONTAINS),
        None => {
            if options.whole_word && contains_whole_word(haystack, &term.lower, false) {
                Some(SCORE_CONTAINS - 100)
            } else {
                None
            }
        }
    }
}

/// Case-insensitive substring search.
///
/// An ASCII needle is searched byte by byte, which needs no allocation and
/// stays correct for names that contain other scripts: a UTF-8 continuation
/// byte never equals an ASCII byte.
pub fn contains_fold(haystack: &str, needle_lower: &str) -> bool {
    find_fold(haystack, needle_lower).is_some()
}

/// Where the needle occurs in the haystack, case insensitively.
///
/// The position is what tells an exact match from a prefix match from a match
/// somewhere in the middle, so one pass is enough to score a candidate.
pub fn find_fold(haystack: &str, needle_lower: &str) -> Option<usize> {
    if needle_lower.is_empty() {
        return Some(0);
    }
    if !needle_lower.is_ascii() {
        return haystack.to_lowercase().find(needle_lower);
    }
    let ned = needle_lower.as_bytes();
    if ned.len() > haystack.len() {
        return None;
    }
    let hay = haystack.as_bytes();
    // 一个字节一个字节地比。
    //
    // **这里本来是「用系统的块搜索找首个字节,再比剩下的字节」—— 那样更慢。**
    // 查询的首字节通常是个常见的字母(`clash` 的 `c`、`report` 的 `e`),所以锚点
    // 一次又一次地被找到,每一次都要把名字剩下的部分再搜两遍。在真实的索引上
    // 那样**慢了 30%**;对照的测量在 `perf/benches/find_fold.rs` 里。
    //
    // 下面这个循环在第一个不匹配的字节上就放弃,所以一个不含这个词的名字
    // 只花一次比较。
    'outer: for start in 0..=(hay.len() - ned.len()) {
        for (offset, expected) in ned.iter().enumerate() {
            if hay[start + offset].to_ascii_lowercase() != *expected {
                continue 'outer;
            }
        }
        return Some(start);
    }
    None
}

/// Substring search that requires the match to sit on a word boundary.
///
/// The scan runs over the bytes: an ASCII needle can never match inside a
/// multi byte character, because the continuation bytes of UTF-8 are all
/// above 0x7F.
fn contains_whole_word(haystack: &str, needle: &str, case_sensitive: bool) -> bool {
    if needle.is_empty() {
        return false;
    }
    let hay = haystack.as_bytes();
    let ned = needle.as_bytes();
    if ned.len() > hay.len() {
        return false;
    }
    let mut start = 0usize;
    while start + ned.len() <= hay.len() {
        let hit = if case_sensitive {
            &hay[start..start + ned.len()] == ned
        } else {
            hay[start..start + ned.len()].eq_ignore_ascii_case(ned)
        };
        if hit {
            let before_ok = start == 0 || !is_word_byte(hay[start - 1]);
            let after_ok = start + ned.len() == hay.len() || !is_word_byte(hay[start + ned.len()]);
            if before_ok && after_ok {
                return true;
            }
        }
        start += 1;
    }
    false
}

/// True for a byte that is part of a word.
///
/// Every byte above 0x7F belongs to a character of another script, and
/// those are letters as far as the matcher is concerned.
fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80
}

/// Match a glob pattern against the whole string.
///
/// The matcher walks the strings directly, without collecting them into
/// vectors of characters: a wildcard query over a million entries would
/// otherwise allocate two million times.
pub fn wildcard_match(pattern: &str, text: &str, case_sensitive: bool) -> bool {
    let mut pi = 0usize;
    let mut ti = 0usize;
    // Where the last `*` was, so the matcher can let it swallow one more
    // character when something does not line up.
    let mut star_pattern = 0usize;
    let mut star_text = 0usize;
    let mut has_star = false;

    while ti < text.len() {
        let pattern_char = pattern[pi..].chars().next();
        let text_char = text[ti..].chars().next();
        let text_len = text_char.map(|c| c.len_utf8()).unwrap_or(0);
        match pattern_char {
            Some('*') => {
                has_star = true;
                star_pattern = pi;
                star_text = ti;
                pi += 1;
            }
            Some('?') => {
                pi += 1;
                ti += text_len;
            }
            Some(c) => {
                let hit = match text_char {
                    Some(t) if case_sensitive => c == t,
                    Some(t) => c.to_lowercase().eq(t.to_lowercase()),
                    None => false,
                };
                if hit {
                    pi += c.len_utf8();
                    ti += text_len;
                    continue;
                }
                if !has_star {
                    return false;
                }
                let swallowed = text[star_text..].chars().next().map(|c| c.len_utf8()).unwrap_or(0);
                star_text += swallowed;
                pi = star_pattern + 1;
                ti = star_text;
            }
            None => {
                if !has_star {
                    return false;
                }
                let swallowed = text[star_text..].chars().next().map(|c| c.len_utf8()).unwrap_or(0);
                star_text += swallowed;
                pi = star_pattern + 1;
                ti = star_text;
            }
        }
    }

    // What is left of the pattern may only be stars.
    while pi < pattern.len() {
        match pattern[pi..].chars().next() {
            Some('*') => pi += 1,
            _ => return false,
        }
    }
    true
}

/// Match a glob pattern anywhere inside the string.
pub fn wildcard_contains(pattern: &str, text: &str, case_sensitive: bool) -> bool {
    if pattern.is_empty() {
        return true;
    }
    let mut start = 0usize;
    loop {
        if wildcard_match_at(pattern, text, start, case_sensitive) {
            return true;
        }
        match text[start..].chars().next() {
            Some(c) => start += c.len_utf8(),
            None => return false,
        }
    }
}

/// The glob matcher, starting at a byte offset in the text.
fn wildcard_match_at(pattern: &str, text: &str, from: usize, case_sensitive: bool) -> bool {
    wildcard_match(pattern, &text[from..], case_sensitive)
}

/// Evaluate a `function:` filter.
fn match_function(
    name: &str,
    op: CompareOp,
    value: &str,
    value2: Option<&str>,
    target: &dyn SearchTarget,
    options: &SearchOptions,
) -> bool {
    match name {
        "size" => match_size(op, value, value2, target),
        "type" => match_type(value, target),
        "shell" => match_shell(value, target),
        "child" | "childcount" | "dupe" | "sizedupe" | "content" | "runcount" => {
            // These need to look at more than the entry itself; the index
            // answers them.
            true
        }
        "dm" | "datemodified" => match_date(op, value, value2, target.modified()),
        "dc" | "datecreated" => match_date(op, value, value2, target.created()),
        "da" | "dateaccessed" => match_date(op, value, value2, target.accessed()),
        "ext" => match_ext(op, value, target),
        "len" => match_len(op, value, target),
        "attrib" | "attributes" => match_attrib(value, target),
        "depth" | "parents" => match_depth(op, value, target),
        "parent" | "infolder" => {
            let wanted = normalize_path(value);
            let path = target.path();
            let path = normalize_path(&path);
            if op == CompareOp::Eq {
                path.starts_with(&wanted)
            } else {
                true
            }
        }
        "nosubfolders" => {
            let wanted = normalize_path(value);
            let path = normalize_path(&target.path());
            path.starts_with(&wanted) && !path[wanted.len()..].contains('\\')
        }
        "startwith" => target.name().starts_with(value),
        "endwith" => target.name().ends_with(value),
        "empty" => !target.is_dir() && target.size() == 0,
        "root" => target.depth() == 0,
        "count" => true,
        _ => {
            // Unknown functions never exclude a result: an unsupported filter
            // must not silently hide every file.
            let _ = options;
            true
        }
    }
}

/// Match the kind of an entry.
///
/// Everything knows `file`, `folder` and `documents` here.
fn match_type(value: &str, target: &dyn SearchTarget) -> bool {
    match value.trim().to_lowercase().as_str() {
        "file" | "files" => !target.is_dir(),
        "folder" | "folders" | "directory" => target.is_dir(),
        "documents" => {
            // The kinds of files that hold documents.
            let name = target.name().to_lowercase();
            let extension = name.rsplit_once('.').map(|(_, e)| e.to_string());
            match extension {
                Some(extension) => matches!(
                    extension.as_str(),
                    "doc" | "docx" | "pdf" | "rtf" | "odt" | "txt" | "md" | "xls" | "xlsx"
                        | "ppt" | "pptx" | "csv"
                ),
                None => false,
            }
        }
        _ => true,
    }
}

/// Match an entry that lives in one of the known shell folders.
///
/// The names are the ones Windows uses: `desktop`, `documents`, `downloads`,
/// `pictures`, `music`, `videos` and `home`.
fn match_shell(value: &str, target: &dyn SearchTarget) -> bool {
    let wanted = value.trim().to_lowercase();
    let path = target.path().to_lowercase().replace('/', "\\");
    let folder = match wanted.as_str() {
        "desktop" => "\\desktop\\",
        "documents" => "\\documents\\",
        "downloads" => "\\downloads\\",
        "pictures" | "images" => "\\pictures\\",
        "music" => "\\music\\",
        "videos" => "\\videos\\",
        "home" | "profile" | "user" => "\\users\\",
        _ => return true,
    };
    path.contains(folder)
}

/// Compare the file size against `value`.
fn match_size(op: CompareOp, value: &str, value2: Option<&str>, target: &dyn SearchTarget) -> bool {
    let Some(size) = parse_size(value) else {
        return true;
    };
    match op {
        CompareOp::Eq => match value2 {
            Some(upper) => {
                let upper = parse_size(upper).unwrap_or(u64::MAX);
                target.size() >= size && target.size() <= upper
            }
            None => target.size() == size,
        },
        CompareOp::Lt => target.size() < size,
        CompareOp::Le => target.size() <= size,
        CompareOp::Gt => target.size() > size,
        CompareOp::Ge => target.size() >= size,
    }
}

/// Compare a timestamp against `value`.
fn match_date(op: CompareOp, value: &str, value2: Option<&str>, stamp: u64) -> bool {
    let Some(when) = parse_date(value) else {
        return true;
    };
    match op {
        CompareOp::Eq => match value2 {
            Some(upper) => {
                let upper = parse_date(upper).unwrap_or(u64::MAX);
                stamp >= when && stamp <= upper
            }
            None => same_day(stamp, when),
        },
        CompareOp::Lt => stamp < when,
        CompareOp::Le => stamp <= when,
        CompareOp::Gt => stamp > when,
        CompareOp::Ge => stamp >= when,
    }
}

/// True when two timestamps fall on the same calendar day.
fn same_day(a: u64, b: u64) -> bool {
    let a = a / 86_400;
    let b = b / 86_400;
    a == b
}

/// Match the file extension against a `;` separated list.
fn match_ext(op: CompareOp, value: &str, target: &dyn SearchTarget) -> bool {
    let name = target.name();
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_lowercase());
    let Some(ext) = ext else {
        return op != CompareOp::Eq;
    };
    let wanted = value.trim_start_matches('.').to_lowercase();
    let hit = wanted
        .split(';')
        .map(str::trim)
        .any(|candidate| candidate.trim_start_matches('.') == ext);
    match op {
        CompareOp::Eq => hit,
        _ => !hit,
    }
}

/// Match the length of the file name.
fn match_len(op: CompareOp, value: &str, target: &dyn SearchTarget) -> bool {
    let Ok(len) = value.trim().parse::<u64>() else {
        return true;
    };
    let name_len = target.name().chars().count() as u64;
    match op {
        CompareOp::Eq => name_len == len,
        CompareOp::Lt => name_len < len,
        CompareOp::Le => name_len <= len,
        CompareOp::Gt => name_len > len,
        CompareOp::Ge => name_len >= len,
    }
}

/// Match a single file attribute letter (`R`, `H`, `S`, …).
fn match_attrib(value: &str, target: &dyn SearchTarget) -> bool {
    let attrs = target.attributes();
    for letter in value.chars() {
        let bit = match letter.to_ascii_uppercase() {
            'A' => 0x20,   // FILE_ATTRIBUTE_ARCHIVE
            'C' => 0x800,  // FILE_ATTRIBUTE_COMPRESSED
            'D' => 0x10,   // FILE_ATTRIBUTE_DIRECTORY
            'E' => 0x4000, // FILE_ATTRIBUTE_ENCRYPTED
            'H' => 0x2,    // FILE_ATTRIBUTE_HIDDEN
            'I' => 0x1000, // FILE_ATTRIBUTE_NOT_CONTENT_INDEXED
            'L' => 0x400,  // FILE_ATTRIBUTE_REPARSE_POINT
            'O' => 0x1000, // FILE_ATTRIBUTE_OFFLINE
            'P' => 0x400,  // FILE_ATTRIBUTE_SPARSE_FILE
            'R' => 0x1,    // FILE_ATTRIBUTE_READONLY
            'S' => 0x4,    // FILE_ATTRIBUTE_SYSTEM
            'T' => 0x100,  // FILE_ATTRIBUTE_TEMPORARY
            'V' => 0x1,    // reserved
            _ => continue,
        };
        if attrs & bit == 0 {
            return false;
        }
    }
    true
}

/// Match the folder depth.
fn match_depth(op: CompareOp, value: &str, target: &dyn SearchTarget) -> bool {
    let Ok(depth) = value.trim().parse::<u64>() else {
        return true;
    };
    let actual = target.depth() as u64;
    match op {
        CompareOp::Eq => actual == depth,
        CompareOp::Lt => actual < depth,
        CompareOp::Le => actual <= depth,
        CompareOp::Gt => actual > depth,
        CompareOp::Ge => actual >= depth,
    }
}

/// Normalise a path for comparison: lower case, back slashes, no trailing slash.
fn normalize_path(path: &str) -> String {
    let mut out = path.replace('/', "\\").to_lowercase();
    while out.len() > 3 && out.ends_with('\\') {
        out.pop();
    }
    out
}

/// Parse a size such as `1024`, `10kb`, `1.5mb`.
///
/// Returns `None` for the named constants, which have no single value.
pub fn parse_size(raw: &str) -> Option<u64> {
    let text = raw.trim().to_lowercase();
    if text.is_empty() {
        return None;
    }
    let (number, multiplier) = if let Some(rest) = text.strip_suffix("kb") {
        (rest, 1024u64)
    } else if let Some(rest) = text.strip_suffix("mb") {
        (rest, 1024 * 1024)
    } else if let Some(rest) = text.strip_suffix("gb") {
        (rest, 1024 * 1024 * 1024)
    } else if let Some(rest) = text.strip_suffix("tb") {
        (rest, 1024u64 * 1024 * 1024 * 1024)
    } else if let Some(rest) = text.strip_suffix('b') {
        (rest, 1)
    } else {
        (text.as_str(), 1)
    };
    let number = number.trim();
    if let Ok(bytes) = number.parse::<u64>() {
        return Some(bytes.saturating_mul(multiplier));
    }
    // `1.5mb`
    if let Some((whole, fraction)) = number.split_once('.') {
        if let (Ok(w), Ok(f)) = (whole.parse::<u64>(), fraction.parse::<u64>()) {
            let scale = 10u64.pow(fraction.len() as u32);
            let fraction = f * multiplier / scale;
            return Some(w.saturating_mul(multiplier).saturating_add(fraction));
        }
    }
    None
}

/// Parse a date into seconds since the Unix epoch.
///
/// Understands `YYYY`, `YYYY-MM`, `YYYY-MM-DD`, `YYYY/MM/DD` and the relative
/// names `today`, `yesterday`, `thisweek`, `lastmonth` and friends.
pub fn parse_date(raw: &str) -> Option<u64> {
    let text = raw.trim().to_lowercase();
    if text.is_empty() {
        return None;
    }
    if let Some(stamp) = parse_relative_date(&text) {
        return Some(stamp);
    }
    // ISO-like forms.
    let parts: Vec<&str> = text.split(['-', '/']).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return None;
    }
    let year: i64 = parts[0].parse().ok()?;
    let month: u32 = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(1);
    let day: u32 = parts.get(2).and_then(|p| p.parse().ok()).unwrap_or(1);
    let (hour, minute, second) = match parts.len() {
        3 => (0, 0, 0),
        _ => {
            if parts.len() > 3 {
                let time = parts[3];
                let mut it = time.split(':');
                (
                    it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
                    it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
                    it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
                )
            } else {
                (0, 0, 0)
            }
        }
    };
    Some(days_from_civil(year, month, day) * 86_400 + hour as u64 * 3600 + minute as u64 * 60 + second as u64)
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> u64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let m = month as u64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe as i64 - 719_468;
    if days < 0 { 0 } else { days as u64 }
}

/// The current time in seconds since the Unix epoch.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Parse the relative date names Everything accepts.
fn parse_relative_date(text: &str) -> Option<u64> {
    let today = now() / 86_400 * 86_400;
    let day = 86_400u64;
    match text {
        "today" => Some(today),
        "yesterday" => Some(today - day),
        "tomorrow" => Some(today + day),
        "thisweek" | "this week" => Some(today - 6 * day),
        "lastweek" | "last week" => Some(today - 13 * day),
        "nextweek" | "next week" => Some(today + day),
        "thismonth" | "this month" => Some(today - 29 * day),
        "lastmonth" | "last month" => Some(today - 59 * day),
        "nextmonth" | "next month" => Some(today + day),
        "thisyear" | "this year" => Some(today - 364 * day),
        "lastyear" | "last year" => Some(today - 729 * day),
        _ => None,
    }
}

/// A recursive descent parser over the query text.
struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn new(query: &str) -> Self {
        Self {
            chars: query.chars().collect(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn skip_spaces(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_whitespace()) {
            self.pos += 1;
        }
    }

    /// `and := unary ( <implicit AND> unary )*`
    fn parse_and(&mut self) -> Node {
        let mut children = Vec::new();
        loop {
            self.skip_spaces();
            match self.peek() {
                None => break,
                Some('|') | Some('>') => break,
                Some('<') => {
                    self.pos += 1;
                    children.push(self.parse_or());
                    self.skip_spaces();
                    if self.peek() == Some('>') {
                        self.pos += 1;
                    }
                }
                _ => {
                    let node = self.parse_unary();
                    if node != Node::All {
                        children.push(node);
                    }
                    // An explicit AND operator.
                    self.skip_spaces();
                    if self.peek() == Some('&') {
                        self.pos += 1;
                    }
                    // Stop when the next token belongs to an outer level.
                    if matches!(self.peek(), None | Some('|') | Some('>')) {
                        break;
                    }
                    // A `&` that we did not consume above means the next token
                    // starts a new conjunct.
                }
            }
        }
        match children.len() {
            0 => Node::All,
            1 => children.pop().unwrap(),
            _ => Node::And(children),
        }
    }

    /// `or := and ( '|' and )*`
    fn parse_or(&mut self) -> Node {
        let mut children = Vec::new();
        loop {
            let node = self.parse_and();
            if node != Node::All {
                children.push(node);
            }
            self.skip_spaces();
            if self.peek() == Some('|') {
                self.pos += 1;
                continue;
            }
            break;
        }
        match children.len() {
            0 => Node::All,
            1 => children.pop().unwrap(),
            _ => Node::Or(children),
        }
    }

    /// `unary := '!' unary | term`
    fn parse_unary(&mut self) -> Node {
        self.skip_spaces();
        match self.peek() {
            Some('!') => {
                self.pos += 1;
                // `!` alone is Everything's "no extension" shortcut.
                if matches!(self.peek(), None | Some(' ') | Some('|') | Some('>')) {
                    return Node::Term(Term::new("!"));
                }
                Node::Not(Box::new(self.parse_unary()))
            }
            _ => self.parse_term(),
        }
    }

    /// Parse the value that follows a modifier, e.g. the `album:^[a-n]` in
    /// `regex:album:^[a-n]`.
    fn parse_value(&self, rest: &str) -> Node {
        let mut parser = Parser::new(rest);
        parser.parse_term()
    }

    /// `term := [ 'name:' ] [ '"' value '"' ]`
    fn parse_term(&mut self) -> Node {
        let mut raw = String::new();
        let mut quoted = false;
        // True while the next character sits directly behind a `:`. Only then
        // may a `<` or `>` be part of the term, as the comparison operator of
        // `size:>1mb`; everywhere else they delimit a group.
        let mut after_colon = false;
        while let Some(c) = self.peek() {
            if c == '"' {
                self.pos += 1;
                quoted = true;
                while let Some(c) = self.bump() {
                    if c == '"' {
                        break;
                    }
                    raw.push(c);
                }
                break;
            }
            if c.is_whitespace() || c == '|' {
                break;
            }
            if (c == '<' || c == '>') && !after_colon {
                break;
            }
            after_colon = c == ':';
            raw.push(c);
            self.pos += 1;
        }

        if raw.is_empty() {
            return Node::All;
        }

        // Split a leading `name:` prefix from the value.
        let (prefix, rest) = match raw.split_once(':') {
            Some((head, tail)) => (head.to_lowercase(), tail.to_string()),
            None => (String::new(), raw.clone()),
        };

        if !prefix.is_empty() {
            if let Some((_, flag)) = FLAGS.iter().find(|(name, _)| *name == prefix) {
                // `path:value` and `case:ABC` carry a term as well as the mode
                // switch, so they become an AND of the two.
                if !rest.is_empty() && !is_predicate(*flag) {
                    let term = self.parse_value(&rest);
                    return Node::And(vec![Node::Flag(*flag), term]);
                }
                return Node::Flag(*flag);
            }
            if FUNCTIONS.contains(&prefix.as_str()) {
                let (op, value) = CompareOp::parse(&rest);
                let (value, value2) = split_range(value);
                return Node::Function {
                    name: prefix,
                    op,
                    value,
                    value2,
                };
            }
            if let Some((_, expansion)) = MACROS.iter().find(|(name, _)| *name == prefix) {
                // A macro lists alternatives, so the parts are OR-ed together.
                return parse_macro_terms(expansion);
            }
        }

        let _ = quoted;
        Node::Term(Term::new(raw))
    }
}

/// True when a modifier filters by kind instead of switching a matching mode.
///
/// `file:` and `folder:` narrow the result set; `case:` and friends change how
/// the remaining terms are compared.
fn is_predicate(flag: Flag) -> bool {
    matches!(
        flag,
        Flag::FilesOnly | Flag::FoldersOnly | Flag::NoFilesOnly | Flag::NoFoldersOnly
    )
}

/// Parse a space separated list of terms as an OR group.
fn parse_macro_terms(text: &str) -> Node {
    let terms: Vec<Node> = text
        .split_whitespace()
        .map(|part| {
            let mut parser = Parser::new(part);
            parser.parse_term()
        })
        .collect();
    match terms.len() {
        0 => Node::All,
        1 => terms.into_iter().next().unwrap(),
        _ => Node::Or(terms),
    }
}

/// Split `a..b` or `a-b` into a range.
fn split_range(value: &str) -> (String, Option<String>) {
    if let Some((low, high)) = value.split_once("..") {
        return (low.trim().to_string(), Some(high.trim().to_string()));
    }
    // A single dash is a range separator only when the left side has no date
    // or dash in it already, so `dm:1/8/2014-31/8/2014` survives intact.
    if let Some((low, high)) = value.split_once('-') {
        if !low.contains('/') && !low.contains(':') && !high.is_empty() {
            return (low.trim().to_string(), Some(high.trim().to_string()));
        }
    }
    (value.trim().to_string(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal target used by the tests.
    struct TestTarget {
        name: String,
        path: String,
        dir: bool,
        size: u64,
        modified: u64,
    }

    impl SearchTarget for TestTarget {
        fn name(&self) -> &str {
            &self.name
        }
        fn parent_name(&self) -> &str {
            ""
        }
        fn path(&self) -> String {
            self.path.clone()
        }
        fn is_dir(&self) -> bool {
            self.dir
        }
        fn size(&self) -> u64 {
            self.size
        }
        fn created(&self) -> u64 {
            0
        }
        fn modified(&self) -> u64 {
            self.modified
        }
        fn accessed(&self) -> u64 {
            0
        }
        fn attributes(&self) -> u32 {
            0
        }
        fn depth(&self) -> u32 {
            1
        }
    }

    fn file(name: &str) -> TestTarget {
        TestTarget {
            name: name.to_string(),
            path: format!("C:\\Users\\me\\{name}"),
            dir: false,
            size: 1024,
            modified: 1_700_000_000,
        }
    }

    fn matches(query: &str, target: &TestTarget) -> bool {
        let parsed = ParsedSearch::parse(query);
        evaluate(&parsed.node, target, &parsed.options)
    }

    #[test]
    fn empty_query_matches_everything() {
        let parsed = ParsedSearch::parse("");
        assert!(parsed.is_empty());
        assert!(matches("", &file("a.txt")));
    }

    #[test]
    fn plain_term_matches_substring() {
        assert!(matches("note", &file("notes.txt")));
        assert!(!matches("zzz", &file("notes.txt")));
    }

    #[test]
    fn term_is_case_insensitive_by_default() {
        assert!(matches("NOTES", &file("notes.txt")));
        let parsed = ParsedSearch::parse("case:NOTES");
        assert!(parsed.options.case_sensitive);
        assert!(!evaluate(
            &parsed.node,
            &file("notes.txt"),
            &parsed.options
        ));
    }

    #[test]
    fn space_is_and() {
        assert!(matches("note txt", &file("notes.txt")));
        assert!(!matches("note zzz", &file("notes.txt")));
    }

    #[test]
    fn pipe_is_or() {
        assert!(matches("zzz|note", &file("notes.txt")));
        assert!(!matches("zzz|qqq", &file("notes.txt")));
    }

    #[test]
    fn bang_is_not() {
        assert!(!matches("!note", &file("notes.txt")));
        assert!(matches("!zzz", &file("notes.txt")));
    }

    #[test]
    fn angle_brackets_group() {
        let parsed = ParsedSearch::parse("<zzz|note> txt");
        assert!(matches("<zzz|note> txt", &file("notes.txt")));
        assert_eq!(parsed.node, Node::And(vec![
            Node::Or(vec![
                Node::Term(Term::new("zzz")),
                Node::Term(Term::new("note")),
            ]),
            Node::Term(Term::new("txt")),
        ]));
    }

    #[test]
    fn file_and_folder_filters() {
        let mut dir = file("pictures");
        dir.dir = true;
        assert!(matches("file:", &file("a.txt")));
        assert!(!matches("file:", &dir));
        assert!(matches("folder:", &dir));
        assert!(!matches("folder:", &file("a.txt")));
    }

    #[test]
    fn size_filter_compares() {
        // The fixture is exactly 1024 bytes.
        assert!(matches("size:>512", &file("a.txt")));
        assert!(matches("size:>=1kb", &file("a.txt")));
        assert!(!matches("size:>1kb", &file("a.txt")));
        assert!(!matches("size:>1mb", &file("a.txt")));
        assert!(matches("size:1kb", &file("a.txt")));
        assert!(matches("size:<2kb", &file("a.txt")));
    }

    #[test]
    fn size_supports_units_and_ranges() {
        assert_eq!(parse_size("1kb"), Some(1024));
        assert_eq!(parse_size("2mb"), Some(2 * 1024 * 1024));
        assert_eq!(parse_size("1.5kb"), Some(1536));
        assert!(matches("size:512..2048", &file("a.txt")));
    }

    #[test]
    fn ext_filter_matches() {
        assert!(matches("ext:txt", &file("a.txt")));
        assert!(!matches("ext:doc", &file("a.txt")));
        assert!(matches("ext:doc;txt", &file("a.txt")));
    }

    #[test]
    fn wildcard_matches_whole_name() {
        assert!(matches("*.txt", &file("a.txt")));
        assert!(!matches("*.doc", &file("a.txt")));
    }

    #[test]
    fn quotes_protect_spaces() {
        assert!(matches("\"notes txt\"", &file("notes txt")));
    }

    #[test]
    fn macros_expand() {
        assert!(matches("exe:", &file("a.exe")));
        assert!(!matches("exe:", &file("a.txt")));
    }

    #[test]
    fn path_modifier_switches_to_the_path() {
        let parsed = ParsedSearch::parse("path:");
        assert!(parsed.options.match_path);
        assert!(evaluate(
            &parsed.node,
            &file("a.txt"),
            &parsed.options
        ));
    }

    #[test]
    fn unknown_function_is_a_literal_term() {
        // Everything treats an unrecognised `name:value` as plain text, so the
        // query searches for the literal string.
        let parsed = ParsedSearch::parse("nosuchfunction:1");
        assert!(matches!(parsed.node, Node::Term(_)));
        assert!(!matches("nosuchfunction:1", &file("a.txt")));
    }

    #[test]
    fn date_filter_uses_relative_names() {
        assert!(parse_date("today").is_some());
        assert!(parse_date("2024").is_some());
        assert_eq!(parse_date("2024-01-02"), Some(days_from_civil(2024, 1, 2) * 86_400));
    }

    #[test]
    fn range_split_keeps_dates_intact() {
        let (low, high) = split_range("1/8/2014-31/8/2014");
        assert_eq!(low, "1/8/2014-31/8/2014");
        assert!(high.is_none());
        let (low, high) = split_range("1mb..2mb");
        assert_eq!(low, "1mb");
        assert_eq!(high.as_deref(), Some("2mb"));
    }
}
