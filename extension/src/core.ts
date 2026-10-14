// The Rust helper (hxp-core) that walks, matches, searches and highlights. One process per
// window, started on first use and kept alive; requests and results are JSON objects, one per line.
import * as cp from 'child_process';
import * as readline from 'readline';
import * as vscode from 'vscode';

/** File picker entry: path relative to its root, absolute path, fuzzy match positions. */
export interface FileItem {
	k?: undefined;
	d: string;
	p: string;
	i?: number[];
}

/** Search result: a file header, followed by its matching lines. */
export interface SearchFile {
	k: 'f';
	d: string;
	p: string;
	/** Matching lines in the file. */
	n: number;
	/** Line of the best match. */
	l: number | null;
}

export interface SearchLine {
	k: 'm';
	d: string;
	p: string;
	l: number;
	x: string;
	r: [number, number][];
	def: boolean;
}

export type Row = FileItem | SearchFile | SearchLine;

export interface Snapshot {
	t: 'snap';
	kind: 'files' | 'search';
	total: number;
	matched: number;
	offset: number;
	items: Row[];
	running: boolean;
	error: string | null;
	lines?: number;
	searched?: number;
}

export interface Highlighted {
	t: 'hl';
	id: number;
	lines: string[] | null;
}

export interface HighlightedRows {
	t: 'hlrows';
	rows: { key: string; html: string }[];
}

export type CoreMessage = Snapshot | Highlighted | HighlightedRows;

export type CoreRequest =
	| { op: 'files'; roots: string[] }
	| { op: 'search'; roots: string[]; active?: string; open: string[]; theme: string }
	| { op: 'query'; text: string }
	| { op: 'view'; offset: number; count: number }
	| { op: 'close' }
	| { op: 'highlight'; id: number; path: string; first: number; last: number; theme: string };

export class Core implements vscode.Disposable {
	private proc: cp.ChildProcess | undefined;
	private listener: ((message: CoreMessage) => void) | undefined;

	constructor(private readonly binary: string, private readonly log: vscode.OutputChannel) {}

	onMessage(listener: (message: CoreMessage) => void): void {
		this.listener = listener;
	}

	send(request: CoreRequest): void {
		this.start().stdin!.write(JSON.stringify(request) + '\n');
	}

	private start(): cp.ChildProcess {
		if (this.proc && this.proc.exitCode === null && !this.proc.killed) {
			return this.proc;
		}
		const runtime = vscode.workspace.getConfiguration('helixPicker').get<string>('helixRuntime', '');
		const proc = cp.spawn(this.binary, runtime ? ['--runtime', runtime] : [], { stdio: ['pipe', 'pipe', 'pipe'] });
		readline.createInterface({ input: proc.stdout! }).on('line', line => {
			let message: { t: string };
			try {
				message = JSON.parse(line);
			} catch {
				this.log.appendLine(`core: unparsable output: ${line}`);
				return;
			}
			if (message.t === 'snap' || message.t === 'hl' || message.t === 'hlrows') {
				this.listener?.(message as CoreMessage);
			} else {
				this.log.appendLine(`core: ${line}`);
			}
		});
		proc.stderr!.on('data', data => this.log.append(`core: ${data}`));
		proc.on('error', err => this.log.appendLine(`core failed to start: ${err.message}`));
		proc.on('exit', code => {
			this.log.appendLine(`core exited with ${code}`);
			if (this.proc === proc) {
				this.proc = undefined;
			}
		});
		this.proc = proc;
		return proc;
	}

	dispose(): void {
		this.proc?.kill();
		this.proc = undefined;
	}
}
