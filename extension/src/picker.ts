// The picker: opens the view (modal overlay or tab), relays results and highlighting from the core
// to the view, prepares previews and opens the chosen file.
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import * as vscode from 'vscode';
import { Core, CoreMessage, Row } from './core';

export const VIEW_TYPE = 'helixPicker.view';
/** Workbench-internal id of the modal editor group; the public ViewColumn cannot address it. */
const MODAL_GROUP = -4;
const PREVIEW_MAX_BYTES = 2 * 1024 * 1024;
/** Lines shown above and below a search hit, and from the top of a file. */
const PREVIEW_BEFORE = 150;
const PREVIEW_AFTER = 250;
const PREVIEW_FILE_LINES = 400;

export type Kind = 'files' | 'search';
type Split = 'none' | 'vertical' | 'horizontal';

interface PickerState {
	kind: Kind;
	query: string;
	selected: number;
}

type ViewMessage =
	| { type: 'ready' }
	| { type: 'query'; text: string }
	| { type: 'view'; offset: number; count: number }
	| { type: 'selected'; index: number }
	| { type: 'preview'; path: string; line?: number }
	| { type: 'open'; item: Row; split: Split }
	| { type: 'close' };

export class Picker implements vscode.CustomReadonlyEditorProvider, vscode.Disposable {
	private panel: vscode.WebviewPanel | undefined;
	private pending: PickerState | undefined;
	private state: PickerState | undefined;
	private roots: string[] = [];
	private returnColumn: vscode.ViewColumn | undefined;
	/** What the user was working on when the picker opened, for ranking search results. */
	private activeFile: string | undefined;
	private openFiles: string[] = [];
	/** Preview highlighting requested from the core, by request id; only the latest counts. */
	private previewRequest: { id: number; path: string; line?: number; start: number } | undefined;
	private previewId = 0;
	private readonly pickerFile: vscode.Uri;

	constructor(private readonly context: vscode.ExtensionContext, private readonly core: Core, private readonly log: vscode.OutputChannel) {
		const dir = context.globalStorageUri.fsPath;
		fs.mkdirSync(dir, { recursive: true });
		this.pickerFile = vscode.Uri.file(path.join(dir, 'picker.helixpicker'));
		if (!fs.existsSync(this.pickerFile.fsPath)) {
			fs.writeFileSync(this.pickerFile.fsPath, '');
		}
		core.onMessage(message => this.onCoreMessage(message));
	}

	async open(kind: Kind, resume?: PickerState): Promise<void> {
		const state = resume ?? { kind, query: '', selected: 0 };
		if (this.panel) {
			this.start(state);
			this.panel.reveal();
			return;
		}
		this.returnColumn = vscode.window.tabGroups.activeTabGroup.viewColumn;
		this.roots = searchRoots();
		this.activeFile = tabPath(vscode.window.tabGroups.activeTabGroup.activeTab);
		this.openFiles = vscode.window.tabGroups.all.flatMap(group => group.tabs.map(tabPath)).filter((p): p is string => !!p);
		this.pending = state;
		const presentation = vscode.workspace.getConfiguration('helixPicker').get<string>('presentation', 'modal');
		if (presentation === 'modal') {
			try {
				await vscode.commands.executeCommand('_workbench.openWith', this.pickerFile, VIEW_TYPE, [MODAL_GROUP, { preserveFocus: false }]);
				return;
			} catch (err) {
				this.log.appendLine(`modal overlay unavailable, opening as tab: ${err}`);
			}
		}
		const panel = vscode.window.createWebviewPanel(VIEW_TYPE + '.tab', 'Picker', { viewColumn: vscode.ViewColumn.Active, preserveFocus: false });
		this.attach(panel);
	}

	resume(): Promise<void> {
		return this.open(this.state?.kind ?? 'files', this.state);
	}

	openCustomDocument(uri: vscode.Uri): vscode.CustomDocument {
		return { uri, dispose() {} };
	}

	resolveCustomEditor(_document: vscode.CustomDocument, panel: vscode.WebviewPanel): void {
		this.attach(panel);
	}

	private attach(panel: vscode.WebviewPanel): void {
		if (!this.pending) {
			// A picker VS Code restored on startup: nothing asked for it.
			setTimeout(() => panel.dispose());
			return;
		}
		this.panel = panel;
		const media = vscode.Uri.joinPath(this.context.extensionUri, 'media');
		panel.webview.options = { enableScripts: true, localResourceRoots: [media] };
		panel.webview.html = this.html(panel.webview, media);
		panel.webview.onDidReceiveMessage((msg: ViewMessage) => this.onMessage(msg));
		panel.onDidDispose(() => {
			if (this.panel === panel) {
				this.panel = undefined;
				this.core.send({ op: 'close' });
			}
		});
	}

	private start(state: PickerState): void {
		this.state = { ...state };
		const fontSize = vscode.workspace.getConfiguration('helixPicker').get<number>('fontSize', 12);
		const scope = this.roots.map(root => root.startsWith(os.homedir()) ? '~' + root.slice(os.homedir().length) : root);
		this.log.appendLine(`${state.kind} in ${this.roots.join(', ')}`);
		this.post({ type: 'init', kind: state.kind, query: state.query, selected: state.selected, fontSize, scope });
		if (state.kind === 'search') {
			this.core.send({ op: 'search', roots: this.roots, active: this.activeFile, open: this.openFiles, theme: helixTheme() });
		} else {
			this.core.send({ op: 'files', roots: this.roots });
		}
		if (state.query) {
			this.core.send({ op: 'query', text: state.query });
		}
	}

	private onMessage(msg: ViewMessage): void {
		switch (msg.type) {
			case 'ready':
				if (this.pending) {
					this.start(this.pending);
					this.pending = undefined;
				}
				break;
			case 'query':
				this.core.send({ op: 'query', text: msg.text });
				if (this.state) {
					this.state.query = msg.text;
					this.state.selected = 0;
				}
				break;
			case 'view':
				this.core.send({ op: 'view', offset: msg.offset, count: msg.count });
				break;
			case 'selected':
				if (this.state) {
					this.state.selected = msg.index;
				}
				break;
			case 'preview':
				this.preview(msg.path, msg.line);
				break;
			case 'open':
				this.openItem(msg.item, msg.split);
				break;
			case 'close':
				this.panel?.dispose();
				break;
		}
	}

	private onCoreMessage(message: CoreMessage): void {
		switch (message.t) {
			case 'snap':
				if (message.kind === this.state?.kind) {
					this.post({ ...message, type: 'snap' });
				}
				break;
			case 'hlrows':
				this.post({ type: 'hlrows', rows: message.rows });
				break;
			case 'hl': {
				const request = this.previewRequest;
				if (request?.id === message.id && message.lines) {
					this.post({ type: 'preview', path: request.path, line: request.line, start: request.start + 1, lines: message.lines });
				}
				break;
			}
		}
	}

	/** Sends the plain preview right away and asks the core to highlight it like Helix. */
	private preview(file: string, line?: number): void {
		const lines = readLines(file);
		if (!lines) {
			this.post({ type: 'preview', path: file, line, binary: true });
			return;
		}
		const start = line ? Math.max(0, line - 1 - PREVIEW_BEFORE) : 0;
		const end = Math.min(lines.length, line ? line - 1 + PREVIEW_AFTER : PREVIEW_FILE_LINES);
		this.post({ type: 'preview', path: file, line, start: start + 1, lines: lines.slice(start, end).map(escape) });
		this.previewRequest = { id: ++this.previewId, path: file, line, start };
		this.core.send({ op: 'highlight', id: this.previewId, path: file, first: start, last: end, theme: helixTheme() });
	}

	private async openItem(item: Row, split: Split): Promise<void> {
		const uri = vscode.Uri.file(item.p);
		this.panel?.dispose();
		if (split === 'vertical') {
			await vscode.commands.executeCommand('workbench.action.newGroupRight');
		} else if (split === 'horizontal') {
			await vscode.commands.executeCommand('workbench.action.newGroupBelow');
		}
		const viewColumn = split === 'none' ? this.returnColumn ?? vscode.ViewColumn.Active : vscode.ViewColumn.Active;
		const line = item.k === undefined ? null : item.l;
		if (line === null) {
			await vscode.commands.executeCommand('vscode.open', uri, { viewColumn, preview: false });
			return;
		}
		const document = await vscode.workspace.openTextDocument(uri);
		const lineIndex = Math.min(line - 1, document.lineCount - 1);
		const match = item.k === 'm' && item.r.length ? item.x.slice(item.r[0][0], item.r[0][1]) : '';
		const column = match ? Math.max(0, document.lineAt(lineIndex).text.indexOf(match)) : 0;
		const selection = new vscode.Range(lineIndex, column, lineIndex, column + match.length);
		await vscode.window.showTextDocument(document, { viewColumn, preview: false, selection });
	}

	private post(message: object): void {
		this.panel?.webview.postMessage(message);
	}

	private html(webview: vscode.Webview, media: vscode.Uri): string {
		const template = fs.readFileSync(path.join(media.fsPath, 'picker.html'), 'utf8');
		return template
			.replaceAll('{{csp}}', webview.cspSource)
			.replaceAll('{{css}}', webview.asWebviewUri(vscode.Uri.joinPath(media, 'picker.css')).toString())
			.replaceAll('{{js}}', webview.asWebviewUri(vscode.Uri.joinPath(media, 'picker.js')).toString());
	}

	dispose(): void {
		this.panel?.dispose();
	}
}

/**
 * The Helix theme for highlighting, as a fallback list for the core: the configured theme, or the
 * Helix theme with the VS Code theme's name (Gruvbox Light Hard -> gruvbox_light_hard), then a
 * light or dark default.
 */
function helixTheme(): string {
	const configured = vscode.workspace.getConfiguration('helixPicker').get<string>('previewTheme', 'auto');
	const vscodeTheme = vscode.workspace.getConfiguration('workbench').get<string>('colorTheme', '');
	const kind = vscode.window.activeColorTheme.kind;
	const dark = kind === vscode.ColorThemeKind.Dark || kind === vscode.ColorThemeKind.HighContrast;
	const preferred = configured !== 'auto' ? configured : vscodeTheme.toLowerCase().replace(/[\s-]+/g, '_');
	return `${preferred},${dark ? 'github_dark' : 'github_light'}`;
}

function tabPath(tab: vscode.Tab | undefined): string | undefined {
	const input = tab?.input;
	const uri = input instanceof vscode.TabInputText || input instanceof vscode.TabInputCustom ? input.uri : undefined;
	return uri?.scheme === 'file' ? uri.fsPath : undefined;
}

/** Workspace folders; without a workspace the folder of the active file, else the home folder. */
function searchRoots(): string[] {
	const folders = (vscode.workspace.workspaceFolders ?? []).filter(f => f.uri.scheme === 'file').map(f => f.uri.fsPath);
	if (folders.length) {
		return folders;
	}
	const active = vscode.window.activeTextEditor?.document.uri;
	return [active?.scheme === 'file' ? path.dirname(active.fsPath) : os.homedir()];
}

function escape(text: string): string {
	return text.replace(/[&<>]/g, c => (c === '&' ? '&amp;' : c === '<' ? '&lt;' : '&gt;'));
}

/** The file's lines, or undefined for binary or unreadable files. */
function readLines(file: string): string[] | undefined {
	let fd: number | undefined;
	try {
		fd = fs.openSync(file, 'r');
		const buffer = Buffer.alloc(PREVIEW_MAX_BYTES);
		const bytes = buffer.subarray(0, fs.readSync(fd, buffer, 0, PREVIEW_MAX_BYTES, 0));
		if (bytes.subarray(0, 8192).includes(0)) {
			return undefined;
		}
		return bytes.toString('utf8').split(/\r?\n/);
	} catch {
		return undefined;
	} finally {
		if (fd !== undefined) {
			fs.closeSync(fd);
		}
	}
}
