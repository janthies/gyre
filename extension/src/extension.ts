import * as path from 'path';
import * as vscode from 'vscode';
import { Core } from './core';
import { Picker, VIEW_TYPE } from './picker';

export function activate(context: vscode.ExtensionContext): void {
	const log = vscode.window.createOutputChannel('Helix Picker');
	const core = new Core(path.join(context.extensionPath, 'bin', 'hxp-core'), log);
	const picker = new Picker(context, core, log);
	context.subscriptions.push(
		log,
		core,
		picker,
		vscode.window.registerCustomEditorProvider(VIEW_TYPE, picker),
		vscode.commands.registerCommand('helixPicker.files', () => picker.open('files')),
		vscode.commands.registerCommand('helixPicker.search', () => picker.open('search')),
		vscode.commands.registerCommand('helixPicker.resume', () => picker.resume()),
	);
}

export function deactivate(): void {}
