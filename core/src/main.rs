//! Backend for helix-picker: the fuzzy file picker and the workspace-wide search.
//!
//! Built on the libraries Helix uses for its pickers: nucleo for fuzzy matching, and ignore and
//! grep-searcher (from ripgrep) for walking and searching the workspace. Requests arrive on stdin
//! and results leave on stdout, one JSON object per line. Results are pushed as snapshots of the
//! window of matches the picker currently shows, so the extension never holds the full list.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use grep_matcher::Matcher as _;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{sinks, BinaryDetection, SearcherBuilder};
use ignore::{WalkBuilder, WalkState};
use nucleo::pattern::{CaseMatching, Normalization};
use nucleo::{Config, Injector, Matcher, Nucleo};
use serde::Deserialize;
use serde_json::{json, Value};

/// Search results beyond this are dropped; nobody scrolls that far in a picker.
const MAX_SEARCH_RESULTS: usize = 100_000;
/// Result lines are cut to this many characters; the preview shows the whole line.
const MAX_LINE_CHARS: usize = 300;
/// A global search restarts only after typing pauses this long.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(80);
/// While results are still streaming in, snapshots go out at most this often.
const STREAM_INTERVAL: Duration = Duration::from_millis(30);

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum Request {
    /// Start a file picker over these folders.
    Files { roots: Vec<PathBuf> },
    /// Start a global search over these folders; the query is a regex.
    Search { roots: Vec<PathBuf> },
    Query { text: String },
    /// The slice of matches the picker shows.
    View { offset: u32, count: u32 },
    Close,
}

enum Event {
    Request(Request),
    /// nucleo finished matching a batch.
    Matched,
    InputClosed,
}

struct Item {
    /// Path relative to its root; shown in the list and matched against the query.
    display: String,
    path: PathBuf,
    /// Set for global search results.
    hit: Option<Hit>,
}

struct Hit {
    line: u64,
    text: String,
    /// Character ranges of the regex matches within `text`.
    ranges: Vec<(u32, u32)>,
}

struct Session {
    search: bool,
    roots: Vec<PathBuf>,
    nucleo: Nucleo<Item>,
    matcher: Matcher,
    query: String,
    /// Cleared by the walker or searcher thread of the current run when it finishes.
    producing: Arc<AtomicBool>,
    /// For global search: when to restart the search after the last keystroke.
    restart_at: Option<Instant>,
    error: Option<String>,
    offset: u32,
    count: u32,
    /// Something the picker shows changed since the last snapshot.
    dirty: bool,
    last_running: bool,
}

struct State {
    tx: Sender<Event>,
    /// Pending notification from nucleo, so a burst of batches sends one event.
    matched_pending: Arc<AtomicBool>,
    /// Bumped for every new walk or search; threads of an older run stop when they see it.
    generation: Arc<AtomicU64>,
    session: Option<Session>,
}

fn main() {
    let (tx, rx) = mpsc::channel();
    let reader_tx = tx.clone();
    thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            match serde_json::from_str::<Request>(&line) {
                Ok(request) => {
                    if reader_tx.send(Event::Request(request)).is_err() {
                        break;
                    }
                }
                Err(err) => emit(&json!({ "t": "error", "msg": format!("bad request: {err}") })),
            }
        }
        let _ = reader_tx.send(Event::InputClosed);
    });

    let mut state = State {
        tx,
        matched_pending: Arc::new(AtomicBool::new(false)),
        generation: Arc::new(AtomicU64::new(0)),
        session: None,
    };
    loop {
        let event = match state.timeout() {
            Some(timeout) => rx.recv_timeout(timeout),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match event {
            Ok(Event::Request(request)) => state.handle(request),
            Ok(Event::Matched) => state.matched_pending.store(false, Ordering::Relaxed),
            Ok(Event::InputClosed) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        state.restart_search_if_due();
        state.refresh();
    }
}

impl State {
    fn handle(&mut self, request: Request) {
        match request {
            Request::Files { roots } => {
                let mut session = self.new_session(false, roots);
                let injector = session.nucleo.injector();
                let run = self.generation.load(Ordering::Relaxed);
                let (generation, producing, roots) = (self.generation.clone(), session.producing.clone(), session.roots.clone());
                thread::spawn(move || walk_files(&roots, injector, &generation, run, &producing));
                session.dirty = true;
                self.session = Some(session);
            }
            Request::Search { roots } => {
                let session = self.new_session(true, roots);
                session.producing.store(false, Ordering::Relaxed);
                self.session = Some(session);
            }
            Request::Query { text } => {
                let Some(session) = self.session.as_mut() else { return };
                if session.search {
                    session.restart_at = Some(Instant::now() + SEARCH_DEBOUNCE);
                } else {
                    let append = text.starts_with(&session.query);
                    session.nucleo.pattern.reparse(0, &text, CaseMatching::Smart, Normalization::Smart, append);
                    session.offset = 0;
                }
                session.query = text;
                session.dirty = true;
            }
            Request::View { offset, count } => {
                let Some(session) = self.session.as_mut() else { return };
                session.offset = offset;
                session.count = count;
                session.dirty = true;
            }
            Request::Close => {
                self.generation.fetch_add(1, Ordering::Relaxed);
                self.session = None;
            }
        }
    }

    fn new_session(&mut self, search: bool, roots: Vec<PathBuf>) -> Session {
        self.generation.fetch_add(1, Ordering::Relaxed);
        let (tx, pending) = (self.tx.clone(), self.matched_pending.clone());
        let notify = Arc::new(move || {
            if !pending.swap(true, Ordering::Relaxed) {
                let _ = tx.send(Event::Matched);
            }
        });
        Session {
            search,
            roots,
            nucleo: Nucleo::new(Config::DEFAULT.match_paths(), notify, None, 1),
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            query: String::new(),
            producing: Arc::new(AtomicBool::new(true)),
            restart_at: None,
            error: None,
            offset: 0,
            count: 0,
            dirty: true,
            last_running: true,
        }
    }

    /// How long the main loop may block: short while results stream in, until the debounce
    /// deadline while a search restart is pending, otherwise until the next request.
    fn timeout(&self) -> Option<Duration> {
        let session = self.session.as_ref()?;
        let mut timeout = None;
        if session.producing.load(Ordering::Relaxed) || session.last_running {
            timeout = Some(STREAM_INTERVAL);
        }
        if let Some(at) = session.restart_at {
            let until = at.saturating_duration_since(Instant::now());
            timeout = Some(timeout.map_or(until, |t: Duration| t.min(until)));
        }
        timeout
    }

    fn restart_search_if_due(&mut self) {
        let Some(session) = self.session.as_mut() else { return };
        if !session.restart_at.is_some_and(|at| Instant::now() >= at) {
            return;
        }
        session.restart_at = None;
        session.error = None;
        session.offset = 0;
        session.dirty = true;
        let run = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        session.nucleo.restart(true);
        session.producing = Arc::new(AtomicBool::new(false));
        if session.query.is_empty() {
            return;
        }
        let matcher = match RegexMatcherBuilder::new().case_smart(true).build(&session.query) {
            Ok(matcher) => matcher,
            Err(err) => {
                session.error = Some(err.to_string());
                return;
            }
        };
        session.producing.store(true, Ordering::Relaxed);
        let injector = session.nucleo.injector();
        let (generation, producing, roots) = (self.generation.clone(), session.producing.clone(), session.roots.clone());
        thread::spawn(move || search(&roots, &matcher, injector, &generation, run, &producing));
    }

    /// Lets nucleo pick up new items and matches, and sends a snapshot if the visible part changed.
    fn refresh(&mut self) {
        let Some(session) = self.session.as_mut() else { return };
        let status = session.nucleo.tick(10);
        let running = status.running || session.producing.load(Ordering::Relaxed);
        if !(status.changed || session.dirty || running != session.last_running) {
            return;
        }
        session.dirty = false;
        session.last_running = running;
        emit(&snapshot(session, running));
    }
}

fn snapshot(session: &mut Session, running: bool) -> Value {
    let snap = session.nucleo.snapshot();
    let matched = snap.matched_item_count();
    let start = session.offset.min(matched);
    let end = start.saturating_add(session.count).min(matched);
    let pattern = snap.pattern().column_pattern(0);
    let mut indices = Vec::new();
    let items: Vec<Value> = snap
        .matched_items(start..end)
        .map(|item| {
            let data = item.data;
            let mut out = json!({ "d": data.display, "p": data.path });
            if !session.search {
                indices.clear();
                pattern.indices(item.matcher_columns[0].slice(..), &mut session.matcher, &mut indices);
                indices.sort_unstable();
                indices.dedup();
                out["i"] = json!(indices);
            }
            if let Some(hit) = &data.hit {
                out["l"] = json!(hit.line);
                out["x"] = json!(hit.text);
                out["r"] = json!(hit.ranges);
            }
            out
        })
        .collect();
    json!({
        "t": "snap",
        "kind": if session.search { "search" } else { "files" },
        "total": snap.item_count(),
        "matched": matched,
        "offset": start,
        "items": items,
        "running": running,
        "error": session.error,
    })
}

fn emit(value: &Value) {
    let mut out = io::stdout().lock();
    let _ = serde_json::to_writer(&mut out, value);
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

/// Walks the roots like Helix's file picker: hidden files skipped, .gitignore and .ignore honored,
/// symbolic links followed (ignore detects link loops).
fn walker(roots: &[PathBuf]) -> WalkBuilder {
    let mut builder = WalkBuilder::new(&roots[0]);
    for root in &roots[1..] {
        builder.add(root);
    }
    builder.hidden(true).require_git(false).follow_links(true);
    builder
}

fn display_path(path: &Path, roots: &[PathBuf]) -> String {
    for root in roots {
        if let Ok(rel) = path.strip_prefix(root) {
            if roots.len() == 1 {
                return rel.to_string_lossy().into_owned();
            }
            let name = root.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
            return format!("{name}/{}", rel.to_string_lossy());
        }
    }
    path.to_string_lossy().into_owned()
}

fn push(injector: &Injector<Item>, item: Item) {
    injector.push(item, |item, columns| columns[0] = item.display.as_str().into());
}

fn walk_files(roots: &[PathBuf], injector: Injector<Item>, generation: &AtomicU64, run: u64, producing: &AtomicBool) {
    walker(roots).build_parallel().run(|| {
        let injector = injector.clone();
        Box::new(move |entry| {
            if generation.load(Ordering::Relaxed) != run {
                return WalkState::Quit;
            }
            let Ok(entry) = entry else { return WalkState::Continue };
            if entry.file_type().is_some_and(|t| t.is_file()) {
                let path = entry.into_path();
                push(&injector, Item { display: display_path(&path, roots), path, hit: None });
            }
            WalkState::Continue
        })
    });
    producing.store(false, Ordering::Relaxed);
}

fn search(roots: &[PathBuf], matcher: &RegexMatcher, injector: Injector<Item>, generation: &AtomicU64, run: u64, producing: &AtomicBool) {
    let found = AtomicUsize::new(0);
    walker(roots).build_parallel().run(|| {
        let (injector, matcher, found) = (injector.clone(), matcher.clone(), &found);
        let mut searcher = SearcherBuilder::new()
            .binary_detection(BinaryDetection::quit(b'\x00'))
            .line_number(true)
            .build();
        Box::new(move |entry| {
            if generation.load(Ordering::Relaxed) != run || found.load(Ordering::Relaxed) >= MAX_SEARCH_RESULTS {
                return WalkState::Quit;
            }
            let Ok(entry) = entry else { return WalkState::Continue };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                return WalkState::Continue;
            }
            let path = entry.path();
            let display = display_path(path, roots);
            let _ = searcher.search_path(&matcher, path, sinks::Lossy(|line, text| {
                if generation.load(Ordering::Relaxed) != run {
                    return Ok(false);
                }
                let (text, ranges) = clip_line(&matcher, text);
                let hit = Hit { line, text, ranges };
                push(&injector, Item { display: display.clone(), path: path.to_path_buf(), hit: Some(hit) });
                Ok(found.fetch_add(1, Ordering::Relaxed) + 1 < MAX_SEARCH_RESULTS)
            }));
            WalkState::Continue
        })
    });
    producing.store(false, Ordering::Relaxed);
}

/// Strips leading whitespace and the line ending, cuts long lines, and returns the regex match
/// ranges as character offsets into the shortened line.
fn clip_line(matcher: &RegexMatcher, line: &str) -> (String, Vec<(u32, u32)>) {
    let line = line.trim_end_matches(['\n', '\r']).trim_start();
    let cut = line.char_indices().nth(MAX_LINE_CHARS).map_or(line.len(), |(i, _)| i);
    let text = &line[..cut];
    let mut ranges = Vec::new();
    let _ = matcher.find_iter(text.as_bytes(), |m| {
        if m.start() < m.end() {
            let start = text[..m.start()].chars().count() as u32;
            let len = text[m.start()..m.end()].chars().count() as u32;
            ranges.push((start, start + len));
        }
        true
    });
    (text.to_owned(), ranges)
}
