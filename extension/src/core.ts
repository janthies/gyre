// The Rust helper (hxp-core) that walks, matches and searches. One process per window, started
// on first use and kept alive; requests and results are JSON objects, one per line.
import * as cp from 'child_process';
import * as readline from 'readline';
import * as vscode from 'vscode';

export interface CoreItem {
	/** Path relative to its root, as shown. */
	d: string;
	/** Absolute path. */
	p: string;
	/** Fuzzy match positions in `d` (file picker). */
	i?: number[];
	/** Line number, line text and regex match ranges (global search). */
	l?: number;
	x?: string;
	r?: [number, number][];
}

export interface Snapshot {
	t: 'snap';
	kind: 'files' | 'search';
	total: number;
	matched: number;
	offset: number;
	items: CoreItem[];
	running: boolean;
	error: string | null;
}

export type CoreRequest =
	| { op: 'files' | 'search'; roots: string[] }
	| { op: 'query'; text: string }
	| { op: 'view'; offset: number; count: number }
	| { op: 'close' };

export class Core implements vscode.Disposable {
	private proc: cp.ChildProcess | undefined;
	private listener: ((snapshot: Snapshot) => void) | undefined;

	constructor(private readonly binary: string, private readonly log: vscode.OutputChannel) {}

	onSnapshot(listener: (snapshot: Snapshot) => void): void {
		this.listener = listener;
	}

	send(request: CoreRequest): void {
		this.start().stdin!.write(JSON.stringify(request) + '\n');
	}

	private start(): cp.ChildProcess {
		if (this.proc && this.proc.exitCode === null && !this.proc.killed) {
			return this.proc;
		}
		const proc = cp.spawn(this.binary, [], { stdio: ['pipe', 'pipe', 'pipe'] });
		readline.createInterface({ input: proc.stdout! }).on('line', line => {
			let msg: { t: string };
			try {
				msg = JSON.parse(line);
			} catch {
				this.log.appendLine(`core: unparsable output: ${line}`);
				return;
			}
			if (msg.t === 'snap') {
				this.listener?.(msg as Snapshot);
			} else {
				this.log.appendLine(`core: ${line}`);
			}
		});
		proc.stderr!.on('data', data => this.log.append(`core stderr: ${data}`));
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
