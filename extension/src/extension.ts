import * as path from 'path';
import * as vscode from 'vscode';
import { Core } from './core';
import { Picker, VIEW_TYPE } from './picker';

export function activate(context: vscode.ExtensionContext): void {
	const log = vscode.window.createOutputChannel('Gyre');
	const core = new Core(path.join(context.extensionPath, 'bin', 'hxp-core'), log);
	const picker = new Picker(context, core, log);
	core.onMissingRuntime = () => warnMissingHelix(context);
	context.subscriptions.push(
		log,
		core,
		picker,
		vscode.window.registerCustomEditorProvider(VIEW_TYPE, picker),
		vscode.commands.registerCommand('helixPicker.files', () => picker.open('files')),
		vscode.commands.registerCommand('helixPicker.search', () => picker.open('search')),
		vscode.commands.registerCommand('helixPicker.symbols', () => picker.open('symbols')),
		vscode.commands.registerCommand('helixPicker.resume', () => picker.resume()),
	);
}

/** The preview borrows Helix's grammars and themes; without Helix it stays plain, so say so once. */
async function warnMissingHelix(context: vscode.ExtensionContext): Promise<void> {
	const key = 'missingHelixDismissed';
	if (context.globalState.get<boolean>(key)) {
		return;
	}
	const install = 'How to install Helix';
	const never = "Don't show again";
	const choice = await vscode.window.showWarningMessage(
		'Gyre found no Helix installation, so the preview has no syntax highlighting. Install Helix (for example `sudo snap install helix --classic`), or point `helixPicker.helixRuntime` at its runtime folder.',
		install, never);
	if (choice === install) {
		vscode.env.openExternal(vscode.Uri.parse('https://docs.helix-editor.com/package-managers.html'));
	} else if (choice === never) {
		context.globalState.update(key, true);
	}
}

export function deactivate(): void {}
