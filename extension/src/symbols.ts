// Symbols from the language servers (clangd, rust-analyzer, ...) through VS Code, like Helix's
// symbol pickers: the symbols of the active file, or the workspace's symbols matching a query.
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

/** A file's path relative to the search root it is in, or from the home folder. */
export function displayPath(file: string, roots: string[]): string {
	for (const root of roots) {
		const relative = path.relative(root, file);
		if (relative && !relative.startsWith('..') && !path.isAbsolute(relative)) {
			return roots.length > 1 ? path.join(path.basename(root), relative) : relative;
		}
	}
	return file.startsWith(os.homedir()) ? '~' + file.slice(os.homedir().length) : file;
}

/** The active file's symbols in file order, with nested symbols after their parent. */
export async function documentSymbols(file: string | undefined, roots: string[]): Promise<SymbolItem[]> {
	if (!file) {
		return [];
	}
	const symbols = await vscode.commands.executeCommand<(vscode.DocumentSymbol | vscode.SymbolInformation)[]>(
		'vscode.executeDocumentSymbolProvider', vscode.Uri.file(file)) ?? [];
	const display = displayPath(file, roots);
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
 * The workspace's symbols matching `query`. The language server filters and orders them; on top
 * of that, exact and prefix name matches, functions and types, and files next to the active one
 * come first, backups, tests and build output last.
 */
export async function workspaceSymbols(query: string, roots: string[], active: string | undefined): Promise<SymbolItem[]> {
	const symbols = await vscode.commands.executeCommand<vscode.SymbolInformation[]>('vscode.executeWorkspaceSymbolProvider', query) ?? [];
	const wanted = query.trim().toLowerCase();
	const scored = symbols
		.filter(symbol => symbol.location.uri.scheme === 'file')
		.map((symbol, order) => {
			const file = symbol.location.uri.fsPath;
			const start = symbol.location.range.start;
			const item: SymbolItem = {
				k: 's', name: symbol.name, kind: KIND_LABELS.get(symbol.kind) ?? '', container: symbol.containerName ?? '',
				d: displayPath(file, roots), p: file, l: start.line + 1, c: start.character,
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
