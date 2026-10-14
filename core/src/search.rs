//! Workspace search with relevance ranking.
//!
//! The query is a list of keywords; a file matches if every keyword occurs in its content or path.
//! Files are ranked the way Sourcegraph ranks keyword search (BM25F): each keyword's frequency is
//! saturated, normalized by file length and weighted by how rare the keyword is, and matches in
//! the path or on a definition count five times. Like Zoekt, whole-word matches count more than
//! parts of words, and generated, vendored, test, backup and patch files rank lower.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{sinks, Searcher};
use regex::Regex;

/// Weight of a match in the path or on a definition line (Sourcegraph's filename/symbol boost).
const FIELD_BOOST: f32 = 5.0;
/// Weight of a match inside a longer word.
const PARTIAL_WEIGHT: f32 = 0.5;
/// BM25 parameters: frequency saturation and length normalization.
const K1: f32 = 1.2;
const B: f32 = 0.75;
/// Matches on comment lines count this much.
const COMMENT_WEIGHT: f32 = 0.5;
/// A file that defines a searched name ranks this much higher; Zoekt ranks symbol matches first.
const DEFINITION_FILE_BOOST: f32 = 2.0;
/// Matching lines kept per file, best first; the file's score counts all of them.
pub const MAX_LINES_PER_FILE: usize = 10;
/// Result lines are cut to this many characters; the preview shows the whole line.
const MAX_LINE_CHARS: usize = 300;

pub struct Term {
    /// Finds the term in a line or path.
    regex: Regex,
    /// The term as typed, for literal terms; definitions are only recognized for these.
    literal: Option<String>,
    case_insensitive: bool,
}

pub struct Query {
    pub terms: Vec<Term>,
    /// Any of the terms, for finding candidate lines quickly.
    pub matcher: RegexMatcher,
}

/// Parses `foo bar "a phrase" /re.ex/`: literal keywords, quoted phrases and regexes, each
/// case-insensitive unless it contains an uppercase letter.
pub fn parse(query: &str) -> Result<Query, String> {
    let mut tokens: Vec<(String, bool)> = Vec::new();
    let mut chars = query.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '"' || c == '/' {
            chars.next();
            let mut token = String::new();
            let mut closed = false;
            while let Some(ch) = chars.next() {
                if ch == '\\' && c == '/' {
                    token.push(ch);
                    token.extend(chars.next());
                } else if ch == c {
                    closed = true;
                    break;
                } else {
                    token.push(ch);
                }
            }
            if !closed {
                // An unfinished "phrase or /regex is taken literally until it is closed.
                tokens.push((format!("{c}{token}"), false));
            } else if !token.is_empty() {
                tokens.push((token, c == '/'));
            }
        } else {
            let mut token = String::new();
            while let Some(&ch) = chars.peek() {
                if ch.is_whitespace() {
                    break;
                }
                token.push(ch);
                chars.next();
            }
            tokens.push((token, false));
        }
    }
    let mut terms = Vec::new();
    let mut alternatives = Vec::new();
    for (token, is_regex) in tokens {
        let case_insensitive = !token.chars().any(char::is_uppercase);
        let body = if is_regex { token.clone() } else { regex::escape(&token) };
        let pattern = if case_insensitive { format!("(?i:{body})") } else { format!("(?:{body})") };
        let regex = Regex::new(&pattern).map_err(|err| format!("/{token}/: {err}"))?;
        alternatives.push(pattern);
        terms.push(Term { regex, literal: (!is_regex).then_some(token), case_insensitive });
    }
    let matcher = RegexMatcherBuilder::new().build(&alternatives.join("|")).map_err(|err| err.to_string())?;
    Ok(Query { terms, matcher })
}

pub struct LineHit {
    pub line: u64,
    pub text: String,
    /// Character ranges of the matches within `text`.
    pub ranges: Vec<(u32, u32)>,
    pub definition: bool,
}

pub struct FileHit {
    pub display: String,
    pub path: PathBuf,
    size: u64,
    /// Weighted frequency of each term in this file.
    frequencies: Vec<f32>,
    prior: f32,
    pub lines: Vec<LineHit>,
    /// Matching lines, including those beyond MAX_LINES_PER_FILE.
    pub line_count: usize,
}

/// Corpus statistics for BM25, collected while searching.
pub struct Stats {
    pub files: AtomicU32,
    bytes: AtomicU64,
    /// Files containing each term.
    document_frequency: Vec<AtomicU32>,
}

impl Stats {
    pub fn new(terms: usize) -> Self {
        Stats { files: AtomicU32::new(0), bytes: AtomicU64::new(0), document_frequency: (0..terms).map(|_| AtomicU32::new(0)).collect() }
    }
}

/// What the user is working on, for ranking nearby and open files higher.
#[derive(Default)]
pub struct Context {
    pub roots: Vec<PathBuf>,
    pub active: Option<PathBuf>,
    pub open: Vec<PathBuf>,
}

impl Context {
    /// The folder directly below a search root that contains `path`, e.g. `RIOT` in ~/thesis/RIOT/sys.
    fn project<'a>(&self, path: &'a Path) -> Option<std::path::Component<'a>> {
        self.roots.iter().find_map(|root| path.strip_prefix(root).ok()?.components().next())
    }
}

/// Per-thread cache of the definition patterns for each language and term.
#[derive(Default)]
pub struct Definitions(HashMap<&'static str, Vec<Option<Regex>>>);

impl Definitions {
    fn for_file(&mut self, path: &Path, query: &Query) -> &[Option<Regex>] {
        let family = family(path);
        self.0.entry(family).or_insert_with(|| {
            query
                .terms
                .iter()
                .map(|term| {
                    let core = term.literal.as_ref()?.trim_matches(|c: char| !is_word_char(c));
                    let templates = definition_templates(family);
                    if templates.is_empty() || core.is_empty() || !core.chars().all(is_word_char) {
                        return None;
                    }
                    let t = regex::escape(core);
                    let flags = if term.case_insensitive { "(?i)" } else { "" };
                    let joined = templates.iter().map(|tpl| tpl.replace("{t}", &t)).collect::<Vec<_>>().join("|");
                    Regex::new(&format!("{flags}(?:{joined})")).ok()
                })
                .collect()
        })
    }
}

fn family(path: &Path) -> &'static str {
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    if name == "makefile" || name == "gnumakefile" || name.starts_with("makefile.") || name == "cmakelists.txt" {
        return "make";
    }
    if name == "kconfig" || name.starts_with("kconfig.") {
        return "kconfig";
    }
    match path.extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref() {
        Some("c" | "h" | "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" | "ino") => "c",
        Some("rs") => "rust",
        Some("py" | "pyi") => "python",
        Some("js" | "mjs" | "cjs" | "ts" | "mts" | "cts" | "jsx" | "tsx") => "js",
        Some("go") => "go",
        Some("sh" | "bash" | "zsh") => "shell",
        Some("mk" | "mak" | "cmake") => "make",
        Some("md" | "markdown") => "markdown",
        Some("tex" | "sty" | "cls") => "tex",
        Some("dts" | "dtsi" | "overlay") => "devicetree",
        _ => "other",
    }
}

/// Lines that define `{t}` rather than use it, per language family.
fn definition_templates(family: &str) -> &'static [&'static str] {
    match family {
        "c" => &[
            r"^\s*#\s*define\s+{t}\b",
            r"^\s*(?:typedef\s+)?(?:struct|union|enum|class|namespace)\s+{t}\b",
            r"^\s*\}\s*{t}\s*;",
            // Function definitions and prototypes start in column 0; calls are indented
            r"^[A-Za-z_][\w\s\*&:<>,]*\b{t}\s*\(",
        ],
        "rust" => &[r"\b(?:fn|struct|enum|trait|type|mod|const|static|union)\s+{t}\b", r"macro_rules!\s*{t}\b", r"^\s*impl\b.*\b{t}\b"],
        "python" => &[r"^\s*(?:async\s+)?(?:def|class)\s+{t}\b", r"^{t}\s*="],
        "js" => &[r"\b(?:function|class|interface|type|enum|const|let|var)\s+{t}\b"],
        "go" => &[r"^func\s+(?:\([^)]*\)\s*)?{t}\b", r"^type\s+{t}\b"],
        "shell" => &[r"^\s*(?:function\s+)?{t}\s*\(\)", r"^\s*{t}="],
        "make" => &[r"^{t}\s*(?:[:+?]?=|:)", r"^\s*(?:function|macro|set)\s*\(\s*{t}\b"],
        "kconfig" => &[r"^\s*(?:menu)?config\s+{t}\b"],
        "markdown" => &[r"^#{1,6}\s.*{t}"],
        "tex" => &[r"\\(?:chapter|section|subsection|subsubsection|paragraph|label|newcommand|def)\*?\{?[^}]*{t}"],
        "devicetree" => &[r"^\s*{t}\s*[:{]"],
        _ => &[],
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether a match is not part of a longer word. Only edges that are word characters need a
/// boundary: `main(` in `main(void)` is whole, in `exec_main(` it is not.
fn whole_word(line: &str, start: usize, end: usize) -> bool {
    let text = &line[start..end];
    let starts_word = text.chars().next().is_some_and(is_word_char);
    let ends_word = text.chars().next_back().is_some_and(is_word_char);
    !(starts_word && line[..start].chars().next_back().is_some_and(is_word_char)) && !(ends_word && line[end..].chars().next().is_some_and(is_word_char))
}

/// Whether a line is a comment, judged by how it starts.
fn is_comment(family: &str, line: &str) -> bool {
    let line = line.trim_start();
    let slash = line.starts_with("//") || line.starts_with("/*") || line.starts_with("* ") || line == "*";
    match family {
        "c" | "rust" | "js" | "go" | "devicetree" => slash,
        "python" | "shell" | "make" | "kconfig" => line.starts_with('#'),
        "tex" => line.starts_with('%'),
        _ => false,
    }
}

/// Searches one file. Returns the hit if every term occurs in the file's content or path;
/// updates the corpus statistics either way.
pub fn search_file(query: &Query, searcher: &mut Searcher, definitions: &mut Definitions, path: &Path, display: &str, size: u64, stats: &Stats, context: &Context) -> Option<FileHit> {
    stats.files.fetch_add(1, Ordering::Relaxed);
    stats.bytes.fetch_add(size, Ordering::Relaxed);
    let terms = &query.terms;
    let mut frequencies = vec![0.0f32; terms.len()];
    for (i, term) in terms.iter().enumerate() {
        frequencies[i] += FIELD_BOOST * term.regex.find_iter(display).count() as f32;
    }
    let defs = definitions.for_file(path, query).to_vec();
    let family = family(path);
    // (relevance, line) of every matching line; only the best are kept
    let mut lines: Vec<(f32, LineHit)> = Vec::new();
    let mut line_count = 0;
    let _ = searcher.search_path(&query.matcher, path, sinks::Lossy(|number, text| {
        let line = text.trim_end_matches(['\n', '\r']);
        let comment = is_comment(family, line);
        let mut ranges = Vec::new();
        let mut definition = false;
        let mut distinct = 0;
        let mut relevance = 0.0;
        for (i, term) in terms.iter().enumerate() {
            let before = ranges.len();
            for m in term.regex.find_iter(line) {
                if m.start() == m.end() {
                    continue;
                }
                let whole = whole_word(line, m.start(), m.end());
                let is_definition = whole && !comment && defs[i].as_ref().is_some_and(|re| re.is_match(line));
                let mut weight = if is_definition { FIELD_BOOST } else if whole { 1.0 } else { PARTIAL_WEIGHT };
                if comment {
                    weight *= COMMENT_WEIGHT;
                }
                frequencies[i] += weight;
                relevance += weight;
                definition |= is_definition;
                ranges.push((m.start(), m.end()));
            }
            distinct += usize::from(ranges.len() > before);
        }
        if ranges.is_empty() {
            return Ok(true);
        }
        line_count += 1;
        // Lines with more of the searched terms come first, then by weight
        let relevance = distinct as f32 * 100.0 + relevance.min(99.0);
        if lines.len() < MAX_LINES_PER_FILE || relevance > lines.last().map_or(0.0, |l| l.0) {
            let position = lines.partition_point(|l| l.0 >= relevance);
            lines.insert(position, (relevance, clip_line(line, number, ranges, definition)));
            lines.truncate(MAX_LINES_PER_FILE);
        }
        Ok(true)
    }));
    let has_definition = lines.iter().any(|l| l.1.definition);
    let lines: Vec<LineHit> = lines.into_iter().map(|l| l.1).collect();
    for (i, frequency) in frequencies.iter().enumerate() {
        if *frequency > 0.0 {
            stats.document_frequency[i].fetch_add(1, Ordering::Relaxed);
        }
    }
    if frequencies.iter().any(|f| *f == 0.0) {
        return None;
    }
    let prior = prior(display, path, context) * if has_definition { DEFINITION_FILE_BOOST } else { 1.0 };
    Some(FileHit { display: display.to_owned(), path: path.to_path_buf(), size, frequencies, prior, lines, line_count })
}

/// Strips leading whitespace, cuts long lines and turns byte ranges into character ranges.
fn clip_line(line: &str, number: u64, mut ranges: Vec<(usize, usize)>, definition: bool) -> LineHit {
    let indent = line.len() - line.trim_start().len();
    let text = &line[indent..];
    let cut = text.char_indices().nth(MAX_LINE_CHARS).map_or(text.len(), |(i, _)| i);
    let text = &text[..cut];
    ranges.sort_unstable();
    let ranges = ranges
        .into_iter()
        .filter_map(|(start, end)| {
            let (start, end) = (start.checked_sub(indent)?, end.saturating_sub(indent).min(text.len()));
            (start < end).then(|| (text[..start].chars().count() as u32, text[..end].chars().count() as u32))
        })
        .collect();
    LineHit { line: number, text: text.to_owned(), ranges, definition }
}

/// Document-level weight: what kind of file this is, how deep it sits, and whether the user has
/// it open or is working next to it.
fn prior(display: &str, path: &Path, context: &Context) -> f32 {
    let lower = display.to_lowercase();
    let segments: Vec<&str> = lower.split('/').collect();
    let (file, dirs) = segments.split_last().unwrap_or((&"", &[]));
    let mut weight = 1.0;
    if segments.iter().any(|s| s.contains(".bak") || s.contains("backup") || s.ends_with('~') || s.ends_with(".orig") || s.ends_with(".rej") || s.ends_with(".old")) {
        weight *= 0.3;
    }
    if file.ends_with(".patch") || file.ends_with(".diff") {
        weight *= 0.35;
    }
    if dirs.iter().any(|d| matches!(*d, "build" | "dist" | "out" | "target" | "obj" | "generated" | "gen")) || file.contains(".min.") || file.ends_with(".lock") || *file == "package-lock.json" {
        weight *= 0.4;
    }
    if dirs.iter().any(|d| matches!(*d, "vendor" | "third_party" | "third-party" | "3rdparty" | "3rd-party" | "external" | "extern" | "deps" | "node_modules")) {
        weight *= 0.5;
    }
    if dirs.iter().any(|d| matches!(*d, "test" | "tests" | "testing" | "unittests" | "__tests__" | "spec")) || file.starts_with("test_") || file.contains("_test.") || file.contains(".test.") || file.contains(".spec.") {
        weight *= 0.6;
    }
    weight *= 1.0 + 0.3 / segments.len() as f32;
    if context.open.iter().any(|p| p == path) {
        weight *= 1.5;
    }
    if let Some(active) = &context.active {
        if active.parent().is_some_and(|dir| Some(dir) == path.parent()) {
            weight *= 1.3;
        } else if context.project(active).is_some_and(|p| context.project(path) == Some(p)) {
            weight *= 1.1;
        }
    }
    weight
}

/// Orders the hits by BM25F score, best first.
pub fn rank(hits: &[FileHit], stats: &Stats) -> Vec<usize> {
    let files = stats.files.load(Ordering::Relaxed).max(1) as f32;
    let average = (stats.bytes.load(Ordering::Relaxed) as f32 / files).max(1.0);
    let idf: Vec<f32> = stats
        .document_frequency
        .iter()
        .map(|df| {
            let df = df.load(Ordering::Relaxed) as f32;
            (1.0 + (files - df + 0.5) / (df + 0.5)).ln()
        })
        .collect();
    let scores: Vec<f32> = hits
        .iter()
        .map(|hit| {
            let length = 1.0 - B + B * (hit.size as f32 / average);
            let bm25: f32 = hit.frequencies.iter().zip(&idf).map(|(tf, idf)| idf * tf * (K1 + 1.0) / (tf + K1 * length)).sum();
            bm25 * hit.prior
        })
        .collect();
    let mut order: Vec<usize> = (0..hits.len()).collect();
    order.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]).then_with(|| hits[a].display.cmp(&hits[b].display)));
    order
}
