//! Backend for helix-picker: the fuzzy file picker, the workspace-wide search and the preview's
//! syntax highlighting.
//!
//! Built on the libraries Helix uses: nucleo for fuzzy matching, ignore and grep-searcher (from
//! ripgrep) for walking and searching the workspace, and tree-sitter with Helix's own grammars,
//! queries and themes for highlighting. Requests arrive on stdin and results leave on stdout, one
//! JSON object per line. Results are pushed as snapshots of the window of rows the picker
//! currently shows, so the extension never holds the full list.

use std::collections::HashSet;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use grep_searcher::{BinaryDetection, SearcherBuilder};
use ignore::{WalkBuilder, WalkState};
use nucleo::pattern::{CaseMatching, Normalization};
use nucleo::{Config, Injector, Matcher, Nucleo};
use serde::Deserialize;
use serde_json::{json, Value};

mod highlight;
mod search;

/// Matching files beyond this are dropped; nobody scrolls that far in a picker.
const MAX_SEARCH_FILES: usize = 10_000;
/// A search restarts only after typing pauses this long.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(100);
/// While results are still streaming in, snapshots go out at most this often.
const STREAM_INTERVAL: Duration = Duration::from_millis(30);
/// While a search runs, results are re-ranked at most this often, so rows don't jump on every frame.
const RANK_INTERVAL: Duration = Duration::from_millis(250);
/// Files larger than this are not read for highlighting.
const MAX_HIGHLIGHT_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum Request {
    /// Start a file picker over these folders; `hidden` includes files whose names start with a dot.
    Files {
        roots: Vec<PathBuf>,
        #[serde(default)]
        hidden: bool,
    },
    /// Start a workspace search over these folders. `active` and `open` are the user's current
    /// and open files, for ranking; `theme` is the Helix theme for highlighting result lines.
    Search {
        roots: Vec<PathBuf>,
        #[serde(default)]
        hidden: bool,
        #[serde(default)]
        active: Option<PathBuf>,
        #[serde(default)]
        open: Vec<PathBuf>,
        theme: String,
        #[serde(default = "no_change")]
        saturation: f32,
        #[serde(default)]
        bold_keywords: bool,
    },
    Query { text: String },
    /// The slice of rows the picker shows.
    View { offset: u32, count: u32 },
    Close,
    /// Highlight lines `first..last` (0-based) of a file for the preview.
    Highlight {
        id: u64,
        path: PathBuf,
        first: usize,
        last: usize,
        theme: String,
        #[serde(default = "no_change")]
        saturation: f32,
        #[serde(default)]
        bold_keywords: bool,
    },
}

fn no_change() -> f32 {
    1.0
}

enum Event {
    Request(Request),
    /// nucleo finished matching a batch.
    Matched,
    InputClosed,
}

enum Job {
    Preview { id: u64, path: PathBuf, first: usize, last: usize, theme: String, options: highlight::ThemeOptions },
    /// Result lines: key, file (for its language) and text.
    Rows { theme: String, options: highlight::ThemeOptions, rows: Vec<(String, PathBuf, String)> },
}

/// A file picker entry.
struct Item {
    /// Path relative to its root; shown in the list and matched against the query.
    display: String,
    path: PathBuf,
}

struct SearchRun {
    hits: Arc<Mutex<Vec<search::FileHit>>>,
    stats: Arc<search::Stats>,
}

enum Mode {
    Files {
        nucleo: Nucleo<Item>,
        matcher: Matcher,
    },
    Search {
        context: Arc<search::Context>,
        theme: String,
        theme_options: highlight::ThemeOptions,
        run: Option<SearchRun>,
        /// Ranked order of the first `ranked` hits; later hits follow in arrival order.
        order: Vec<usize>,
        ranked: usize,
        ranked_at: Instant,
        /// Result lines already sent for highlighting in this run.
        highlighted: HashSet<String>,
    },
}

struct Session {
    mode: Mode,
    roots: Vec<PathBuf>,
    hidden: bool,
    query: String,
    /// Cleared by the walker or searcher thread of the current run when it finishes.
    producing: Arc<AtomicBool>,
    /// For search: when to restart after the last keystroke.
    restart_at: Option<Instant>,
    error: Option<String>,
    offset: u32,
    count: u32,
    /// Something the picker shows changed since the last snapshot.
    dirty: bool,
    last_running: bool,
    last_hits: usize,
}

struct State {
    tx: Sender<Event>,
    jobs: Sender<Job>,
    /// Pending notification from nucleo, so a burst of batches sends one event.
    matched_pending: Arc<AtomicBool>,
    /// Bumped for every new walk or search; threads of an older run stop when they see it.
    generation: Arc<AtomicU64>,
    session: Option<Session>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let runtime = args.iter().position(|a| a == "--runtime").and_then(|i| args.get(i + 1)).map(PathBuf::from);
    let dirs = highlight::runtime_dirs(runtime.as_deref());

    // Debugging aid: `hxp-core --highlight FILE [FIRST LAST [THEME [SATURATION [bold]]]]` prints
    // the highlighted lines.
    if args.get(1).map(String::as_str) == Some("--highlight") {
        let path = Path::new(&args[2]);
        let first = args.get(3).and_then(|a| a.parse().ok()).unwrap_or(0);
        let last = args.get(4).and_then(|a| a.parse().ok()).unwrap_or(40);
        let mut highlighter = highlight::Highlighter::new(dirs);
        let options = highlight::ThemeOptions {
            saturation: args.get(6).and_then(|a| a.parse().ok()).unwrap_or(1.0),
            bold_keywords: args.get(7).is_some_and(|a| a == "bold"),
        };
        highlighter.set_theme(args.get(5).map_or("gruvbox_light_hard", String::as_str), options);
        let source = std::fs::read_to_string(path).unwrap_or_default();
        match highlight::lang_for(path).and_then(|lang| highlighter.highlight(lang, &source, first, last)) {
            Some(lines) => lines.iter().for_each(|l| println!("{l}")),
            None => println!("(no highlighting)"),
        }
        return;
    }

    // Without a Helix runtime the preview cannot be highlighted; the extension tells the user once.
    if dirs.is_empty() {
        emit(&json!({ "t": "runtime", "found": false }));
    }

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
    let (jobs, job_rx) = mpsc::channel();
    thread::spawn(move || highlight_worker(job_rx, dirs));

    let mut state = State {
        tx,
        jobs,
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
            Request::Files { roots, hidden } => {
                let notify = {
                    let (tx, pending) = (self.tx.clone(), self.matched_pending.clone());
                    Arc::new(move || {
                        if !pending.swap(true, Ordering::Relaxed) {
                            let _ = tx.send(Event::Matched);
                        }
                    })
                };
                let nucleo = Nucleo::new(Config::DEFAULT.match_paths(), notify, None, 1);
                let injector = nucleo.injector();
                let session = self.new_session(Mode::Files { nucleo, matcher: Matcher::new(Config::DEFAULT.match_paths()) }, roots, hidden);
                let run = self.generation.load(Ordering::Relaxed);
                let (generation, producing, roots) = (self.generation.clone(), session.producing.clone(), session.roots.clone());
                thread::spawn(move || walk_files(&roots, hidden, injector, &generation, run, &producing));
                self.session = Some(session);
            }
            Request::Search { roots, hidden, active, open, theme, saturation, bold_keywords } => {
                let context = Arc::new(search::Context { roots: roots.clone(), active, open });
                let theme_options = highlight::ThemeOptions { saturation, bold_keywords };
                let mode = Mode::Search { context, theme, theme_options, run: None, order: Vec::new(), ranked: 0, ranked_at: Instant::now(), highlighted: HashSet::new() };
                let session = self.new_session(mode, roots, hidden);
                session.producing.store(false, Ordering::Relaxed);
                self.session = Some(session);
            }
            Request::Query { text } => {
                let Some(session) = self.session.as_mut() else { return };
                match &mut session.mode {
                    Mode::Search { .. } => session.restart_at = Some(Instant::now() + SEARCH_DEBOUNCE),
                    Mode::Files { nucleo, .. } => {
                        let append = text.starts_with(&session.query);
                        nucleo.pattern.reparse(0, &text, CaseMatching::Smart, Normalization::Smart, append);
                        session.offset = 0;
                    }
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
            Request::Highlight { id, path, first, last, theme, saturation, bold_keywords } => {
                let options = highlight::ThemeOptions { saturation, bold_keywords };
                let _ = self.jobs.send(Job::Preview { id, path, first, last, theme, options });
            }
        }
    }

    fn new_session(&mut self, mode: Mode, roots: Vec<PathBuf>, hidden: bool) -> Session {
        self.generation.fetch_add(1, Ordering::Relaxed);
        Session {
            mode,
            roots,
            hidden,
            query: String::new(),
            producing: Arc::new(AtomicBool::new(true)),
            restart_at: None,
            error: None,
            offset: 0,
            count: 0,
            dirty: true,
            last_running: true,
            last_hits: 0,
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
        let Mode::Search { context, run: current, order, ranked, highlighted, .. } = &mut session.mode else { return };
        session.restart_at = None;
        session.error = None;
        session.offset = 0;
        session.dirty = true;
        *current = None;
        order.clear();
        *ranked = 0;
        highlighted.clear();
        let run = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        session.producing = Arc::new(AtomicBool::new(false));
        if session.query.trim().is_empty() {
            return;
        }
        let query = match search::parse(&session.query) {
            Ok(query) if !query.terms.is_empty() => Arc::new(query),
            Ok(_) => return,
            Err(err) => {
                session.error = Some(err);
                return;
            }
        };
        let hits = Arc::new(Mutex::new(Vec::new()));
        let stats = Arc::new(search::Stats::new(query.terms.len()));
        *current = Some(SearchRun { hits: hits.clone(), stats: stats.clone() });
        session.producing.store(true, Ordering::Relaxed);
        let (generation, producing, roots, hidden, context) = (self.generation.clone(), session.producing.clone(), session.roots.clone(), session.hidden, context.clone());
        thread::spawn(move || search_files(&roots, hidden, &query, &context, &hits, &stats, &generation, run, &producing));
    }

    /// Sends a snapshot if the visible part changed.
    fn refresh(&mut self) {
        let Some(session) = self.session.as_mut() else { return };
        let producing = session.producing.load(Ordering::Relaxed);
        let (changed, running) = match &mut session.mode {
            Mode::Files { nucleo, .. } => {
                let status = nucleo.tick(10);
                (status.changed, status.running || producing)
            }
            Mode::Search { run, .. } => {
                let hits = run.as_ref().map_or(0, |r| r.hits.lock().unwrap().len());
                let changed = hits != session.last_hits;
                session.last_hits = hits;
                (changed, producing)
            }
        };
        if !(changed || session.dirty || running != session.last_running) {
            return;
        }
        session.dirty = false;
        session.last_running = running;
        let snapshot = match &session.mode {
            Mode::Files { .. } => files_snapshot(session, running),
            Mode::Search { .. } => search_snapshot(session, running, &self.jobs),
        };
        emit(&snapshot);
    }
}

fn files_snapshot(session: &mut Session, running: bool) -> Value {
    let Mode::Files { nucleo, matcher } = &mut session.mode else { unreachable!() };
    let snap = nucleo.snapshot();
    let matched = snap.matched_item_count();
    let start = session.offset.min(matched);
    let end = start.saturating_add(session.count).min(matched);
    let pattern = snap.pattern().column_pattern(0);
    let mut indices = Vec::new();
    let items: Vec<Value> = snap
        .matched_items(start..end)
        .map(|item| {
            indices.clear();
            pattern.indices(item.matcher_columns[0].slice(..), matcher, &mut indices);
            indices.sort_unstable();
            indices.dedup();
            json!({ "d": item.data.display, "p": item.data.path, "i": indices })
        })
        .collect();
    json!({ "t": "snap", "kind": "files", "total": snap.item_count(), "matched": matched, "offset": start, "items": items, "running": running, "error": session.error })
}

/// Search results as rows: each file as a header row, followed by its matching lines.
fn search_snapshot(session: &mut Session, running: bool, jobs: &Sender<Job>) -> Value {
    let Mode::Search { run, order, ranked, ranked_at, theme, theme_options, highlighted, .. } = &mut session.mode else { unreachable!() };
    let Some(run) = run else {
        return json!({ "t": "snap", "kind": "search", "total": 0, "matched": 0, "offset": 0, "items": [], "running": running, "error": session.error, "lines": 0, "searched": 0 });
    };
    let hits = run.hits.lock().unwrap();
    if hits.len() != *ranked && (!running || ranked_at.elapsed() >= RANK_INTERVAL) {
        *order = search::rank(&hits, &run.stats);
        *ranked = hits.len();
        *ranked_at = Instant::now();
    }
    let files: Vec<usize> = order.iter().copied().chain(*ranked..hits.len()).collect();
    let rows: usize = files.iter().map(|&f| 1 + hits[f].lines.len()).sum();
    let lines: usize = files.iter().map(|&f| hits[f].line_count).sum();
    let start = (session.offset as usize).min(rows);
    let end = (start + session.count as usize).min(rows);
    let mut items = Vec::new();
    let mut to_highlight = Vec::new();
    let mut row = 0;
    for &f in &files {
        let hit = &hits[f];
        let size = 1 + hit.lines.len();
        if row + size <= start {
            row += size;
            continue;
        }
        if row >= end {
            break;
        }
        for i in 0..size {
            if row >= start && row < end {
                if i == 0 {
                    items.push(json!({ "k": "f", "d": hit.display, "p": hit.path, "n": hit.line_count, "l": hit.lines.first().map(|l| l.line) }));
                } else {
                    let line = &hit.lines[i - 1];
                    let key = format!("{}:{}", hit.path.display(), line.line);
                    if highlighted.insert(key.clone()) {
                        to_highlight.push((key, hit.path.clone(), line.text.clone()));
                    }
                    items.push(json!({ "k": "m", "d": hit.display, "p": hit.path, "l": line.line, "x": line.text, "r": line.ranges, "def": line.definition }));
                }
            }
            row += 1;
        }
    }
    if !to_highlight.is_empty() {
        let _ = jobs.send(Job::Rows { theme: theme.clone(), options: *theme_options, rows: to_highlight });
    }
    json!({
        "t": "snap",
        "kind": "search",
        "total": files.len(),
        "matched": rows,
        "offset": start,
        "items": items,
        "running": running,
        "error": session.error,
        "lines": lines,
        "searched": run.stats.files.load(Ordering::Relaxed),
    })
}

fn emit(value: &Value) {
    let mut out = io::stdout().lock();
    let _ = serde_json::to_writer(&mut out, value);
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

/// Highlights previews and result lines on its own thread, so the result list never waits for
/// it. When jobs pile up (fast scrolling), only the newest preview is highlighted.
fn highlight_worker(jobs: Receiver<Job>, dirs: Vec<PathBuf>) {
    let mut highlighter = highlight::Highlighter::new(dirs);
    while let Ok(first) = jobs.recv() {
        let mut preview = None;
        let mut rows = Vec::new();
        for job in std::iter::once(first).chain(jobs.try_iter()) {
            match job {
                Job::Preview { .. } => preview = Some(job),
                Job::Rows { theme, options, rows: more } => rows.push((theme, options, more)),
            }
        }
        if let Some(Job::Preview { id, path, first, last, theme, options }) = preview {
            highlighter.set_theme(&theme, options);
            let lines = read_source(&path).and_then(|source| highlight::lang_for(&path).and_then(|lang| highlighter.highlight(lang, &source, first, last)));
            emit(&json!({ "t": "hl", "id": id, "lines": lines }));
        }
        for (theme, options, rows) in rows {
            highlighter.set_theme(&theme, options);
            let out: Vec<Value> = rows
                .iter()
                .filter_map(|(key, path, text)| {
                    let html = highlighter.highlight(highlight::lang_for(path)?, text, 0, 1)?.into_iter().next()?;
                    Some(json!({ "key": key, "html": html }))
                })
                .collect();
            if !out.is_empty() {
                emit(&json!({ "t": "hlrows", "rows": out }));
            }
        }
    }
}

fn read_source(path: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path).ok()?.take(MAX_HIGHLIGHT_BYTES).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Walks the roots like Helix's file picker: .gitignore and .ignore honored, symbolic links followed
/// (ignore detects link loops), hidden files only if asked for. Version control folders stay out
/// either way, as in Helix.
fn walker(roots: &[PathBuf], hidden: bool) -> WalkBuilder {
    let mut builder = WalkBuilder::new(&roots[0]);
    for root in &roots[1..] {
        builder.add(root);
    }
    builder.hidden(!hidden).require_git(false).follow_links(true);
    builder.filter_entry(|entry| !matches!(entry.file_name().to_str(), Some(".git" | ".hg" | ".svn" | ".jj" | ".pijul")));
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

fn walk_files(roots: &[PathBuf], hidden: bool, injector: Injector<Item>, generation: &AtomicU64, run: u64, producing: &AtomicBool) {
    walker(roots, hidden).build_parallel().run(|| {
        let injector = injector.clone();
        Box::new(move |entry| {
            if generation.load(Ordering::Relaxed) != run {
                return WalkState::Quit;
            }
            let Ok(entry) = entry else { return WalkState::Continue };
            if entry.file_type().is_some_and(|t| t.is_file()) {
                let path = entry.into_path();
                let item = Item { display: display_path(&path, roots), path };
                injector.push(item, |item, columns| columns[0] = item.display.as_str().into());
            }
            WalkState::Continue
        })
    });
    producing.store(false, Ordering::Relaxed);
}

#[allow(clippy::too_many_arguments)]
fn search_files(roots: &[PathBuf], hidden: bool, query: &search::Query, context: &search::Context, hits: &Mutex<Vec<search::FileHit>>, stats: &search::Stats, generation: &AtomicU64, run: u64, producing: &AtomicBool) {
    walker(roots, hidden).build_parallel().run(|| {
        let mut searcher = SearcherBuilder::new().binary_detection(BinaryDetection::quit(b'\x00')).line_number(true).build();
        let mut definitions = search::Definitions::default();
        Box::new(move |entry| {
            if generation.load(Ordering::Relaxed) != run {
                return WalkState::Quit;
            }
            let Ok(entry) = entry else { return WalkState::Continue };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                return WalkState::Continue;
            }
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            let path = entry.path();
            let display = display_path(path, roots);
            if let Some(hit) = search::search_file(query, &mut searcher, &mut definitions, path, &display, size, stats, context) {
                let mut hits = hits.lock().unwrap();
                hits.push(hit);
                if hits.len() >= MAX_SEARCH_FILES {
                    return WalkState::Quit;
                }
            }
            WalkState::Continue
        })
    });
    producing.store(false, Ordering::Relaxed);
}
