//! Syntax highlighting like Helix: the tree-sitter grammars, highlight queries and themes of the
//! installed Helix, loaded from its runtime directory. The output is HTML, one string per line.

use std::cell::RefCell;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use libloading::Library;
use regex::Regex;
use tree_sitter::Language;
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter as TsHighlighter};
use tree_sitter_language::LanguageFn;

/// Larger files are shown without highlighting.
const MAX_BYTES: usize = 2 * 1024 * 1024;

pub struct Lang {
    /// Helix's language name, also used for injections.
    name: &'static str,
    grammar: &'static str,
    /// Directory under runtime/queries.
    queries: &'static str,
    extensions: &'static [&'static str],
    file_names: &'static [&'static str],
    aliases: &'static [&'static str],
}

const fn lang(name: &'static str, extensions: &'static [&'static str]) -> Lang {
    Lang { name, grammar: name, queries: name, extensions, file_names: &[], aliases: &[] }
}

/// The languages helix-picker highlights, with the file types and injection names Helix uses for them.
const LANGS: &[Lang] = &[
    lang("c", &["c", "h"]),
    Lang { aliases: &["c++"], ..lang("cpp", &["cpp", "cc", "cxx", "hpp", "hh", "hxx", "ino"]) },
    Lang { aliases: &["rs"], ..lang("rust", &["rs"]) },
    Lang { aliases: &["py", "python3"], ..lang("python", &["py", "pyi"]) },
    Lang { aliases: &["sh", "shell", "zsh"], file_names: &[".bashrc", ".zshrc", ".profile"], ..lang("bash", &["sh", "bash", "zsh"]) },
    Lang { aliases: &["md"], ..lang("markdown", &["md", "markdown"]) },
    Lang { grammar: "markdown_inline", ..lang("markdown.inline", &[]) },
    lang("toml", &["toml"]),
    lang("json", &["json", "jsonc"]),
    Lang { aliases: &["yml"], ..lang("yaml", &["yaml", "yml"]) },
    Lang { aliases: &["makefile"], file_names: &["makefile", "gnumakefile"], ..lang("make", &["mk", "mak"]) },
    Lang { file_names: &["cmakelists.txt"], ..lang("cmake", &["cmake"]) },
    Lang { aliases: &["tex"], ..lang("latex", &["tex", "sty", "cls"]) },
    lang("bibtex", &["bib"]),
    lang("diff", &["diff", "patch"]),
    Lang { aliases: &["dts"], ..lang("devicetree", &["dts", "dtsi", "overlay"]) },
    lang("ld", &["ld", "lds"]),
    Lang { aliases: &["asm", "s"], ..lang("gas", &["s"]) },
    lang("nasm", &["asm", "nasm"]),
    Lang { aliases: &["js"], ..lang("javascript", &["js", "mjs", "cjs"]) },
    Lang { aliases: &["ts"], ..lang("typescript", &["ts", "mts", "cts"]) },
    lang("html", &["html", "htm"]),
    lang("css", &["css"]),
    lang("go", &["go"]),
    lang("java", &["java"]),
    lang("lua", &["lua"]),
    lang("xml", &["xml", "svg"]),
    lang("ini", &["ini", "cfg", "conf"]),
    Lang { file_names: &["dockerfile"], ..lang("dockerfile", &[]) },
    lang("just", &["just"]),
    lang("ron", &["ron"]),
    lang("regex", &[]),
    lang("comment", &[]),
];

pub fn lang_for(path: &Path) -> Option<&'static Lang> {
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    if let Some(lang) = LANGS.iter().find(|l| l.file_names.contains(&name.as_str())) {
        return Some(lang);
    }
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    LANGS.iter().find(|l| l.extensions.contains(&ext.as_str()))
}

fn lang_by_name(name: &str) -> Option<&'static Lang> {
    let name = name.trim().to_lowercase();
    LANGS.iter().find(|l| l.name == name || l.aliases.contains(&name.as_str()) || l.extensions.contains(&name.as_str()))
}

/// Helix's runtime directories, searched in this order like Helix does: a configured one, then
/// $HELIX_RUNTIME, the user's runtime, and the usual install locations.
pub fn runtime_dirs(configured: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    dirs.extend(configured.map(Path::to_path_buf));
    dirs.extend(env::var_os("HELIX_RUNTIME").map(PathBuf::from));
    if let Some(home) = env::var_os("HOME") {
        dirs.push(Path::new(&home).join(".config/helix/runtime"));
    }
    for dir in ["/snap/helix/current/bin/runtime", "/usr/lib/helix/runtime", "/usr/share/helix/runtime", "/usr/local/lib/helix/runtime", "/usr/local/share/helix/runtime", "/opt/helix/runtime"] {
        dirs.push(PathBuf::from(dir));
    }
    dirs.retain(|d| d.is_dir());
    dirs.dedup();
    dirs
}

/// Highlight names the theme styles (for example `keyword.control`) and their CSS.
struct Theme {
    names: Vec<String>,
    styles: Vec<String>,
}

impl Theme {
    fn empty() -> Self {
        Theme { names: Vec::new(), styles: Vec::new() }
    }
}

pub struct Highlighter {
    dirs: Vec<PathBuf>,
    theme_name: String,
    theme: Theme,
    /// Per language; None if its grammar or queries could not be loaded.
    configs: HashMap<&'static str, Option<HighlightConfiguration>>,
    /// Loaded grammars; they must stay loaded while their languages are used.
    libraries: Vec<Library>,
    ts: TsHighlighter,
}

impl Highlighter {
    pub fn new(dirs: Vec<PathBuf>) -> Self {
        Highlighter { dirs, theme_name: String::new(), theme: Theme::empty(), configs: HashMap::new(), libraries: Vec::new(), ts: TsHighlighter::new() }
    }

    /// `names` is a comma-separated list of Helix themes; the first one that exists is used.
    pub fn set_theme(&mut self, names: &str) {
        if names == self.theme_name {
            return;
        }
        self.theme_name = names.to_owned();
        self.theme = names.split(',').find_map(|name| self.load_theme(name.trim())).unwrap_or_else(|| {
            eprintln!("none of the helix themes {names:?} found");
            Theme::empty()
        });
        for config in self.configs.values_mut().flatten() {
            config.configure(&self.theme.names);
        }
    }

    /// Highlights `source` and returns the HTML of lines `first..last` (0-based), or None if the
    /// language cannot be highlighted.
    pub fn highlight(&mut self, lang: &'static Lang, source: &str, first: usize, last: usize) -> Option<Vec<String>> {
        if source.len() > MAX_BYTES || !self.ensure(lang) {
            return None;
        }
        // Injected languages (code blocks in Markdown, ...) are loaded when a pass asks for them,
        // then the pass is repeated.
        for _ in 0..4 {
            let missing = RefCell::new(Vec::new());
            let lines = {
                let configs = &self.configs;
                let config = configs.get(lang.name)?.as_ref()?;
                let events = self
                    .ts
                    .highlight(config, source.as_bytes(), None, |name| {
                        let lang = lang_by_name(name)?;
                        match configs.get(lang.name) {
                            Some(config) => config.as_ref(),
                            None => {
                                missing.borrow_mut().push(lang);
                                None
                            }
                        }
                    })
                    .ok()?;
                render(events, source, &self.theme, first, last)
            };
            let mut loaded = false;
            for lang in missing.into_inner() {
                loaded |= self.ensure(lang);
            }
            if !loaded {
                return Some(lines);
            }
        }
        None
    }

    fn ensure(&mut self, lang: &'static Lang) -> bool {
        if !self.configs.contains_key(lang.name) {
            let config = self.load_config(lang);
            self.configs.insert(lang.name, config);
        }
        self.configs[lang.name].is_some()
    }

    fn find(&self, relative: &str) -> Option<PathBuf> {
        self.dirs.iter().map(|d| d.join(relative)).find(|p| p.exists())
    }

    fn load_config(&mut self, lang: &'static Lang) -> Option<HighlightConfiguration> {
        let language = self.load_grammar(lang.grammar)?;
        let highlights = self.read_query(lang.queries, "highlights.scm");
        let injections = self.read_query(lang.queries, "injections.scm");
        // Helix names local definitions by kind (`@local.definition.variable.parameter`);
        // tree-sitter-highlight only knows `@local.definition`.
        let locals = Regex::new(r"@local\.definition\.[\w.-]+").unwrap().replace_all(&self.read_query(lang.queries, "locals.scm"), "@local.definition").into_owned();
        let mut config = match HighlightConfiguration::new(language.clone(), lang.name, &highlights, &injections, &locals) {
            Ok(config) => config,
            Err(err) => {
                eprintln!("{} queries: {err}; retrying without injections and locals", lang.name);
                HighlightConfiguration::new(language, lang.name, &highlights, "", "")
                    .map_err(|err| eprintln!("{} highlights: {err}", lang.name))
                    .ok()?
            }
        };
        config.configure(&self.theme.names);
        Some(config)
    }

    fn load_grammar(&mut self, grammar: &str) -> Option<Language> {
        let path = self.find(&format!("grammars/{grammar}.so"))?;
        // SAFETY: the grammar is a tree-sitter parser built by Helix; its entry point takes no
        // arguments and returns the parser's language table.
        unsafe {
            let library = Library::new(&path).map_err(|err| eprintln!("{}: {err}", path.display())).ok()?;
            let symbol = format!("tree_sitter_{}", grammar.replace('-', "_"));
            let entry: unsafe extern "C" fn() -> *const () = *library.get(symbol.as_bytes()).ok()?;
            let language = Language::new(LanguageFn::from_raw(entry));
            let abi = language.abi_version() as u32;
            if !(tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION as u32..=tree_sitter::LANGUAGE_VERSION as u32).contains(&abi) {
                eprintln!("{grammar}: unsupported grammar ABI {abi}");
                return None;
            }
            self.libraries.push(library);
            Some(language)
        }
    }

    /// A query file, preceded by the queries it inherits (`; inherits: c`), as in Helix.
    fn read_query(&self, queries: &str, file: &str) -> String {
        let Some(text) = self.find(&format!("queries/{queries}/{file}")).and_then(|p| fs::read_to_string(p).ok()) else {
            return String::new();
        };
        let mut out = String::new();
        for line in text.lines() {
            if let Some(parents) = line.trim().strip_prefix("; inherits:") {
                for parent in parents.split(',') {
                    out.push_str(&self.read_query(parent.trim(), file));
                    out.push('\n');
                }
            }
        }
        out.push_str(&text);
        out
    }

    fn load_theme(&self, name: &str) -> Option<Theme> {
        let (palette, scopes) = self.load_theme_tables(name, 0)?;
        let color = |value: &toml::Value| -> Option<String> {
            let name = value.as_str()?;
            Some(palette.get(name).and_then(toml::Value::as_str).unwrap_or(name).to_owned())
        };
        let mut theme = Theme::empty();
        for (scope, value) in &scopes {
            if scope.starts_with("ui.") || scope.starts_with("diagnostic") || ["warning", "error", "info", "hint"].contains(&scope.as_str()) {
                continue;
            }
            let mut css = Vec::new();
            let (fg, modifiers) = match value {
                toml::Value::String(_) => (color(value), None),
                toml::Value::Table(table) => (table.get("fg").and_then(color), table.get("modifiers").and_then(toml::Value::as_array)),
                _ => continue,
            };
            if let Some(fg) = fg {
                css.push(format!("color:{fg}"));
            }
            for modifier in modifiers.into_iter().flatten().filter_map(toml::Value::as_str) {
                match modifier {
                    "bold" => css.push("font-weight:bold".into()),
                    "italic" => css.push("font-style:italic".into()),
                    "underlined" => css.push("text-decoration:underline".into()),
                    "crossed_out" => css.push("text-decoration:line-through".into()),
                    "dim" => css.push("opacity:0.7".into()),
                    _ => {}
                }
            }
            if !css.is_empty() {
                theme.names.push(scope.clone());
                theme.styles.push(css.join(";"));
            }
        }
        Some(theme)
    }

    /// The palette and scopes of a theme, merged over the theme it inherits from.
    fn load_theme_tables(&self, name: &str, depth: usize) -> Option<(toml::Table, toml::Table)> {
        if depth > 8 {
            return None;
        }
        let user = env::var_os("HOME").map(|h| Path::new(&h).join(format!(".config/helix/themes/{name}.toml"))).filter(|p| p.exists());
        let path = user.or_else(|| self.find(&format!("themes/{name}.toml")))?;
        let table: toml::Table = toml::from_str(&fs::read_to_string(path).ok()?).ok()?;
        let (mut palette, mut scopes) = match table.get("inherits").and_then(toml::Value::as_str) {
            Some(parent) => self.load_theme_tables(parent, depth + 1)?,
            None => (toml::Table::new(), toml::Table::new()),
        };
        if let Some(own) = table.get("palette").and_then(toml::Value::as_table) {
            palette.extend(own.clone());
        }
        for (key, value) in table {
            if key != "inherits" && key != "palette" {
                scopes.insert(key, value);
            }
        }
        Some((palette, scopes))
    }
}

fn escape(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\r' => {}
            _ => out.push(c),
        }
    }
}

/// Builds one line of HTML, merging neighbouring pieces with the same style into one span.
#[derive(Default)]
struct LineHtml {
    html: String,
    open: Option<usize>,
}

impl LineHtml {
    fn push(&mut self, text: &str, style: Option<usize>, theme: &Theme) {
        if text.is_empty() {
            return;
        }
        if self.open != style {
            if self.open.is_some() {
                self.html.push_str("</span>");
            }
            if let Some(i) = style {
                self.html.push_str("<span style=\"");
                self.html.push_str(&theme.styles[i]);
                self.html.push_str("\">");
            }
            self.open = style;
        }
        escape(text, &mut self.html);
    }

    fn finish(mut self) -> String {
        if self.open.is_some() {
            self.html.push_str("</span>");
        }
        self.html
    }
}

fn render(events: impl Iterator<Item = Result<HighlightEvent, tree_sitter_highlight::Error>>, source: &str, theme: &Theme, first: usize, last: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = LineHtml::default();
    let mut line = 0;
    let mut stack: Vec<usize> = Vec::new();
    'events: for event in events {
        match event {
            Ok(HighlightEvent::HighlightStart(h)) => stack.push(h.0),
            Ok(HighlightEvent::HighlightEnd) => {
                stack.pop();
            }
            Ok(HighlightEvent::Source { start, end }) => {
                let style = stack.last().copied();
                for (n, part) in source[start..end].split('\n').enumerate() {
                    if n > 0 {
                        if line >= first {
                            lines.push(std::mem::take(&mut current).finish());
                        }
                        line += 1;
                        if line >= last {
                            break 'events;
                        }
                    }
                    if line >= first {
                        current.push(part, style, theme);
                    }
                }
            }
            Err(_) => break,
        }
    }
    if line >= first && line < last {
        lines.push(current.finish());
    }
    lines
}
