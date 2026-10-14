// Syntax highlighting for the preview with Shiki, which uses the same TextMate grammars and
// themes as VS Code. Shiki is ESM-only, so it is loaded with a dynamic import on first use.
import * as path from 'path';
import * as vscode from 'vscode';
import type { BundledLanguage, BundledTheme, HighlighterGeneric } from 'shiki' with { 'resolution-mode': 'import' };

type ShikiModule = typeof import('shiki', { with: { 'resolution-mode': 'import' } });
type Highlighter = HighlighterGeneric<BundledLanguage, BundledTheme>;

/** File extensions and names Shiki has no alias for. */
const EXTENSION_LANGS: Record<string, string> = {
	h: 'c', hh: 'cpp', hpp: 'cpp', hxx: 'cpp', cc: 'cpp', cxx: 'cpp', ino: 'cpp',
	mk: 'make', s: 'asm', sty: 'latex', cls: 'latex', bib: 'bibtex',
};
const NAME_LANGS: Record<string, string> = {
	'makefile': 'make', 'gnumakefile': 'make', 'cmakelists.txt': 'cmake', 'dockerfile': 'dockerfile',
};

let shiki: Promise<ShikiModule> | undefined;
let highlighter: Promise<Highlighter> | undefined;

async function load(): Promise<{ module: ShikiModule; highlighter: Highlighter }> {
	shiki ??= import('shiki');
	const module = await shiki;
	highlighter ??= module.createHighlighter({ themes: [], langs: [] }) as Promise<Highlighter>;
	return { module, highlighter: await highlighter };
}

export async function languageOf(file: string): Promise<string | undefined> {
	const { module } = await load();
	const name = path.basename(file).toLowerCase();
	const ext = path.extname(name).slice(1);
	const lang = NAME_LANGS[name] ?? EXTENSION_LANGS[ext] ?? ext;
	return lang in module.bundledLanguages ? lang : undefined;
}

/** The Shiki theme for the current VS Code theme: same name if Shiki has it, else GitHub light/dark. */
async function themeName(): Promise<string> {
	const { module } = await load();
	const configured = vscode.workspace.getConfiguration('helixPicker').get<string>('previewTheme', 'auto');
	if (configured !== 'auto' && configured in module.bundledThemes) {
		return configured;
	}
	const vscodeTheme = vscode.workspace.getConfiguration('workbench').get<string>('colorTheme', '');
	const slug = vscodeTheme.toLowerCase().replace(/\s+/g, '-');
	if (slug in module.bundledThemes) {
		return slug;
	}
	const kind = vscode.window.activeColorTheme.kind;
	const dark = kind === vscode.ColorThemeKind.Dark || kind === vscode.ColorThemeKind.HighContrast;
	return dark ? 'github-dark' : 'github-light';
}

function escape(text: string): string {
	return text.replace(/[&<>]/g, c => (c === '&' ? '&amp;' : c === '<' ? '&lt;' : '&gt;'));
}

export function plainLines(lines: string[]): string[] {
	return lines.map(escape);
}

/** Highlights `lines` as `lang` and returns one HTML string per line. */
export async function highlightLines(lines: string[], lang: string): Promise<string[]> {
	const { highlighter: hl } = await load();
	const theme = await themeName();
	await Promise.all([
		hl.getLoadedThemes().includes(theme) ? undefined : hl.loadTheme(theme as BundledTheme),
		hl.getLoadedLanguages().includes(lang) ? undefined : hl.loadLanguage(lang as BundledLanguage),
	]);
	const { tokens } = hl.codeToTokens(lines.join('\n'), { lang: lang as BundledLanguage, theme: theme as BundledTheme });
	return tokens.map(line => line.map(token => {
		const style = [`color:${token.color}`];
		if (token.fontStyle && token.fontStyle & 1) style.push('font-style:italic');
		if (token.fontStyle && token.fontStyle & 2) style.push('font-weight:bold');
		return `<span style="${style.join(';')}">${escape(token.content)}</span>`;
	}).join(''));
}
