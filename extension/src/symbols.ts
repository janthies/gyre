// Symbols from the language servers (clangd, rust-analyzer, ...) through VS Code, like Helix's
// symbol pickers: the symbols of the active file, or the workspace's symbols matching a query.
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import * as vscode from 'vscode';
import { SymbolItem } from './core';

const KIND_LABELS = new Map<vscode.SymbolKind, string>([
	[vscode.SymbolKind.File, 'file'], [vscode.SymbolKind.Module, 'mod'], [vscode.SymbolKind.Namespace, 'ns'],
	[vscode.SymbolKind.Package, 'pkg'], [vscode.SymbolKind.Class, 'class'], [vscode.SymbolKind.Method, 'method'],
	[vscode.SymbolKind.Property, 'prop'], [vscode.SymbolKind.Field, 'field'], [vscode.SymbolKind.Constructor, 'ctor'],
	[vscode.SymbolKind.Enum, 'enum'], [vscode.SymbolKind.Interface, 'iface'], [vscode.SymbolKind.Function, 'fn'],
	[vscode.SymbolKind.Variable, 'var'], [vscode.SymbolKind.Constant, 'const'], [vscode.SymbolKind.String, 'str'],
	[vscode.SymbolKind.Number, 'num'], [vscode.SymbolKind.Boolean, 'bool'], [vscode.SymbolKind.Array, 'array'],
	[vscode.SymbolKind.Object, 'object'], [vscode.SymbolKind.Key, 'key'], [vscode.SymbolKind.Null, 'null'],
	[vscode.SymbolKind.EnumMember, 'member'], [vscode.SymbolKind.Struct, 'struct'], [vscode.SymbolKind.Event, 'event'],
	[vscode.SymbolKind.Operator, 'op'], [vscode.SymbolKind.TypeParameter, 'type'],
]);

/** Kinds that define something you would jump to: functions and types before variables and fields. */
const DEFINITION_KINDS = new Set(['fn', 'method', 'ctor', 'class', 'struct', 'enum', 'iface', 'type', 'mod', 'ns']);
const C_LANGUAGES = new Set(['c', 'cpp', 'cuda-cpp', 'objective-c', 'objective-cpp']);

export class Symbols {
	/** Real paths of the folders linked directly below the roots, mapped to their linked path. */
	private readonly links = new Map<string, string>();
	private warmedUp = false;

	constructor(private readonly roots: string[]) {
		for (const root of roots) {
			try {
				for (const entry of fs.readdirSync(root, { withFileTypes: true })) {
					if (entry.isSymbolicLink()) {
						const linked = path.join(root, entry.name);
						this.links.set(fs.realpathSync(linked), linked);
					}
				}
			} catch {
				// unreadable root: no links to map
			}
		}
	}

	/**
	 * A file's path relative to the search root it is in, or from the home folder. Language
	 * servers report real paths; files in linked folders are shown below the link.
	 */
	display(file: string): string {
		for (const [real, linked] of this.links) {
			if (file.startsWith(real + path.sep)) {
				file = linked + file.slice(real.length);
				break;
			}
		}
		for (const root of this.roots) {
			const relative = path.relative(root, file);
			if (relative && !relative.startsWith('..') && !path.isAbsolute(relative)) {
				return this.roots.length > 1 ? path.join(path.basename(root), relative) : relative;
			}
		}
		return file.startsWith(os.homedir()) ? '~' + file.slice(os.homedir().length) : file;
	}

	/** The active file's symbols in file order, with nested symbols after their parent. */
	async document(file: string | undefined): Promise<SymbolItem[]> {
		if (!file) {
			return [];
		}
		const symbols = await vscode.commands.executeCommand<(vscode.DocumentSymbol | vscode.SymbolInformation)[]>(
			'vscode.executeDocumentSymbolProvider', vscode.Uri.file(file)) ?? [];
		const display = this.display(file);
		const items: SymbolItem[] = [];
		const visit = (symbol: vscode.DocumentSymbol | vscode.SymbolInformation, container: string) => {
			if ('children' in symbol) {
				const start = symbol.selectionRange.start;
				items.push({ k: 's', name: symbol.name, kind: KIND_LABELS.get(symbol.kind) ?? '', container, d: display, p: file, l: start.line + 1, c: start.character });
				for (const child of symbol.children) {
					visit(child, symbol.name);
				}
			} else {
				const start = symbol.location.range.start;
				items.push({ k: 's', name: symbol.name, kind: KIND_LABELS.get(symbol.kind) ?? '', container: symbol.containerName, d: display, p: file, l: start.line + 1, c: start.character });
			}
		};
		symbols.forEach(symbol => visit(symbol, ''));
		return items.sort((a, b) => a.l - b.l);
	}

	/**
	 * The workspace's symbols matching `query`. The language server filters and orders them; on
	 * top of that, exact and prefix name matches, functions and types, and files next to the
	 * active one come first, backups, tests and build output last.
	 */
	async workspace(query: string, active: string | undefined): Promise<SymbolItem[]> {
		const symbols = await vscode.commands.executeCommand<vscode.SymbolInformation[]>('vscode.executeWorkspaceSymbolProvider', query) ?? [];
		const wanted = query.trim().toLowerCase();
		const scored = symbols
			.filter(symbol => symbol.location.uri.scheme === 'file')
			.map((symbol, order) => {
				const file = symbol.location.uri.fsPath;
				const start = symbol.location.range.start;
				const item: SymbolItem = {
					k: 's', name: symbol.name, kind: KIND_LABELS.get(symbol.kind) ?? '', container: symbol.containerName ?? '',
					d: this.display(file), p: file, l: start.line + 1, c: start.character,
				};
				const name = symbol.name.toLowerCase();
				const where = item.d.toLowerCase();
				let score = name === wanted ? 100 : name.startsWith(wanted) ? 60 : name.includes(wanted) ? 30 : 0;
				score += DEFINITION_KINDS.has(item.kind) ? 10 : 0;
				score -= /\.bak|backup|\.orig$|~$/.test(where) ? 40 : 0;
				score -= /(^|\/)(build|dist|out|target|vendor|third_party|external)\//.test(where) ? 20 : 0;
				score -= /(^|\/)(tests?|testing|unittests)\//.test(where) ? 15 : 0;
				score += active && path.dirname(active) === path.dirname(file) ? 10 : 0;
				return { item, score, order };
			});
		scored.sort((a, b) => b.score - a.score || a.order - b.order);
		return scored.map(s => s.item);
	}

	/**
	 * clangd loads its compile database and index only once a C file is open; until then it finds
	 * no workspace symbols at all. If none is open, this opens (without showing) the first file of
	 * the compile database.
	 */
	async warmUp(): Promise<void> {
		if (this.warmedUp) {
			return;
		}
		this.warmedUp = true;
		if (vscode.workspace.textDocuments.some(d => C_LANGUAGES.has(d.languageId))) {
			return;
		}
		const file = this.compileCommandsFile();
		if (file) {
			await vscode.workspace.openTextDocument(vscode.Uri.file(file));
		}
	}

	/** First file of the compile database clangd uses: from `--compile-commands-dir`, a root or its build folder. */
	private compileCommandsFile(): string | undefined {
		const dirs = [...this.roots, ...this.roots.map(root => path.join(root, 'build'))];
		for (const argument of vscode.workspace.getConfiguration('clangd').get<string[]>('arguments', [])) {
			const match = /^--compile-commands-dir=(.+)$/.exec(argument);
			if (match) {
				dirs.unshift(match[1]);
			}
		}
		for (const dir of dirs) {
			try {
				const entry = JSON.parse(fs.readFileSync(path.join(dir, 'compile_commands.json'), 'utf8'))[0];
				if (entry?.file) {
					return path.isAbsolute(entry.file) ? entry.file : path.join(entry.directory, entry.file);
				}
			} catch {
				// no database here
			}
		}
		return undefined;
	}
}
