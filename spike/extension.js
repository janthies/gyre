// Step 0 spike: can a Helix-style picker open as a centered modal overlay, and how fast is it?
// The same picker opens two ways, and the time until its webview is ready is logged:
//   modal: a custom editor opened into VS Code's modal editor group via the internal
//          _workbench.openWith, because the public ViewColumn cannot address that group
//   tab:   a plain webview panel in the active editor group (public API only)
const vscode = require('vscode');
const fs = require('fs');
const os = require('os');
const path = require('path');

const VIEW_TYPE = 'helixPickerSpike.picker';
const MODAL_GROUP = -4; // workbench-internal id of the modal editor group
const LOG = path.join(os.tmpdir(), 'helix-picker-spike.log');
const PREVIEW_BYTES = 64 * 1024;
const MAX_FILES = 50000;

let extensionUri;
let pickerFile; // empty file the custom editor is opened on
let session = null; // { mode, t0, returnColumn, panel }

function log(line) {
	fs.appendFileSync(LOG, `${new Date().toISOString()} ${line}\n`);
}

function activate(context) {
	extensionUri = context.extensionUri;
	const dir = context.globalStorageUri.fsPath;
	fs.mkdirSync(dir, { recursive: true });
	pickerFile = vscode.Uri.file(path.join(dir, 'picker.helixpicker'));
	fs.writeFileSync(pickerFile.fsPath, '');

	context.subscriptions.push(
		vscode.window.registerCustomEditorProvider(VIEW_TYPE, {
			openCustomDocument: uri => ({ uri, dispose() {} }),
			resolveCustomEditor: (_document, panel) => attach(panel),
		}),
		vscode.commands.registerCommand('helixPickerSpike.openModal', () => open('modal')),
		vscode.commands.registerCommand('helixPickerSpike.openTab', () => open('tab')),
	);
}

async function open(mode) {
	session?.panel?.dispose();
	session = {
		mode,
		t0: Date.now(),
		returnColumn: vscode.window.tabGroups.activeTabGroup.viewColumn,
		panel: null,
	};
	log(`${mode}: open requested`);
	try {
		if (mode === 'modal') {
			await vscode.commands.executeCommand('_workbench.openWith', pickerFile, VIEW_TYPE, [MODAL_GROUP, { preserveFocus: false }]);
		} else {
			const panel = vscode.window.createWebviewPanel(VIEW_TYPE + '.tab', 'Picker',
				{ viewColumn: vscode.ViewColumn.Active, preserveFocus: false });
			attach(panel);
		}
	} catch (err) {
		log(`${mode}: failed: ${err}`);
		vscode.window.showErrorMessage(`Helix picker (${mode}) failed: ${err.message ?? err}`);
		session = null;
	}
}

function attach(panel) {
	const s = session ?? { mode: 'restored', t0: Date.now(), returnColumn: vscode.ViewColumn.One };
	s.panel = panel;
	panel.webview.options = { enableScripts: true, localResourceRoots: [extensionUri] };
	panel.webview.html = html();
	panel.onDidDispose(() => {
		if (session === s) {
			session = null;
		}
	});
	panel.webview.onDidReceiveMessage(msg => onMessage(s, msg));
}

async function onMessage(s, msg) {
	const webview = s.panel.webview;
	switch (msg.type) {
		case 'ready': {
			const readyMs = Date.now() - s.t0;
			const groups = vscode.window.tabGroups.all.length;
			log(`${s.mode}: ready after ${readyMs} ms (viewColumn=${s.panel.viewColumn}, tab groups=${groups})`);
			webview.postMessage({ type: 'timing', mode: s.mode, readyMs });

			const t1 = Date.now();
			const uris = await vscode.workspace.findFiles('**/*', '{**/node_modules/**,**/.git/**}', MAX_FILES);
			const files = uris.map(u => vscode.workspace.asRelativePath(u, false)).sort();
			const listMs = Date.now() - t1;
			log(`${s.mode}: ${files.length} files listed in ${listMs} ms`);
			webview.postMessage({ type: 'files', files, listMs });
			break;
		}
		case 'preview': {
			webview.postMessage({ type: 'preview', file: msg.file, text: readPreview(msg.file) });
			break;
		}
		case 'open': {
			const uri = resolve(msg.file);
			s.panel.dispose();
			if (uri) {
				await vscode.window.showTextDocument(uri, {
					viewColumn: msg.split ? vscode.ViewColumn.Beside : s.returnColumn ?? vscode.ViewColumn.Active,
					preview: false,
				});
			}
			break;
		}
		case 'close':
			s.panel.dispose();
			break;
	}
}

function resolve(rel) {
	const folder = vscode.workspace.workspaceFolders?.[0];
	if (!folder) {
		return undefined;
	}
	return path.isAbsolute(rel) ? vscode.Uri.file(rel) : vscode.Uri.joinPath(folder.uri, rel);
}

function readPreview(rel) {
	const uri = resolve(rel);
	if (!uri) {
		return null;
	}
	let fd;
	try {
		fd = fs.openSync(uri.fsPath, 'r');
		const buf = Buffer.alloc(PREVIEW_BYTES);
		const n = fs.readSync(fd, buf, 0, PREVIEW_BYTES, 0);
		const bytes = buf.subarray(0, n);
		return bytes.includes(0) ? null : bytes.toString('utf8');
	} catch {
		return null;
	} finally {
		if (fd !== undefined) {
			fs.closeSync(fd);
		}
	}
}

function html() {
	const nonce = Math.random().toString(36).slice(2);
	return fs.readFileSync(path.join(extensionUri.fsPath, 'picker.html'), 'utf8')
		.replaceAll('{{nonce}}', nonce);
}

function deactivate() {}

module.exports = { activate, deactivate };
