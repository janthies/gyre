// The picker view: prompt, result list and preview. The core sends only the window of results
// that is visible; this script keeps the selection, asks for the window it needs and renders it.
(function () {
	const vscode = acquireVsCodeApi();
	const query = document.getElementById('query');
	const count = document.getElementById('count');
	const message = document.getElementById('message');
	const list = document.getElementById('list');
	const previewTitle = document.getElementById('preview-title');
	const previewBody = document.getElementById('preview-body');

	/** Pause before asking for a preview, so scrolling fast doesn't request every file. */
	const PREVIEW_DELAY = 40;
	/** Below this width the preview is hidden, as in Helix on a narrow terminal. */
	const NARROW = 760;

	let kind = 'files';
	let matched = 0;
	let total = 0;
	let running = false;
	let error = null;
	/** Items of the last snapshot and the index of its first item. */
	let items = [];
	let itemsOffset = 0;
	let selected = 0;
	let offset = 0;
	let rows = 1;
	let requested = { offset: -1, count: -1 };
	let previewWanted = true;
	let previewKey = null;
	let previewTimer;
	/** Last preview per file and line; the highlighted version replaces the plain one. */
	const previews = new Map();

	function escape(text) {
		return text.replace(/[&<>]/g, c => (c === '&' ? '&amp;' : c === '<' ? '&lt;' : '&gt;'));
	}

	/** Wraps the characters at `indices` (code point positions) in <b>. */
	function markChars(text, indices) {
		if (!indices || !indices.length) {
			return escape(text);
		}
		const marked = new Set(indices);
		return Array.from(text, (c, i) => (marked.has(i) ? `<b>${escape(c)}</b>` : escape(c))).join('');
	}

	/** Wraps the code point ranges [start, end) in <b>. */
	function markRanges(text, ranges) {
		const chars = Array.from(text);
		let out = '';
		let pos = 0;
		for (const [start, end] of ranges || []) {
			out += escape(chars.slice(pos, start).join('')) + `<b>${escape(chars.slice(start, end).join(''))}</b>`;
			pos = end;
		}
		return out + escape(chars.slice(pos).join(''));
	}

	function renderItem(item) {
		if (item.l === undefined) {
			return markChars(item.d, item.i);
		}
		return `<span class="location">${escape(item.d)}:${item.l}</span>  ${markRanges(item.x, item.r)}`;
	}

	function itemAt(index) {
		return items[index - itemsOffset];
	}

	function visibleRows() {
		const rowHeight = parseFloat(getComputedStyle(document.body).fontSize) * 1.6;
		return Math.max(1, Math.floor((list.clientHeight - 8) / rowHeight));
	}

	/** Scrolls the list so the selection is visible and asks the core for that window. */
	function requestView(force) {
		rows = visibleRows();
		if (selected < offset) {
			offset = selected;
		} else if (selected >= offset + rows) {
			offset = selected - rows + 1;
		}
		offset = Math.max(0, Math.min(offset, matched - rows));
		if (force || offset !== requested.offset || rows !== requested.count) {
			requested = { offset, count: rows };
			vscode.postMessage({ type: 'view', offset, count: rows });
		}
	}

	function renderMessage() {
		let text = '';
		if (error) {
			text = error;
		} else if (kind === 'search' && !query.value) {
			text = 'Type a regex to search the workspace.';
		} else if (!running && matched === 0 && (total > 0 || kind === 'search')) {
			text = 'No matches.';
		}
		message.textContent = text;
		message.classList.toggle('visible', !!text);
		message.classList.toggle('error', !!error);
	}

	function render() {
		count.textContent = `${matched}/${total}`;
		count.classList.toggle('running', running);
		renderMessage();
		let html = '';
		for (let i = 0; i < rows && offset + i < matched; i++) {
			const index = offset + i;
			const item = itemAt(index);
			html += `<li class="${index === selected ? 'selected' : ''}">${item ? renderItem(item) : ''}</li>`;
		}
		list.innerHTML = html;
		schedulePreview();
	}

	function select(index) {
		const clamped = Math.max(0, Math.min(index, matched - 1));
		if (clamped === selected) {
			return;
		}
		selected = clamped;
		requestView(false);
		render();
		vscode.postMessage({ type: 'selected', index: selected });
	}

	function previewVisible() {
		return previewWanted && window.innerWidth >= NARROW;
	}

	function updateLayout() {
		document.body.classList.toggle('no-preview', !previewVisible());
	}

	function schedulePreview() {
		clearTimeout(previewTimer);
		const item = itemAt(selected);
		if (!item) {
			if (matched === 0) {
				previewKey = null;
				previewTitle.textContent = '';
				previewBody.innerHTML = '';
			}
			return;
		}
		if (!previewVisible()) {
			return;
		}
		const key = `${item.p}:${item.l ?? ''}`;
		// The title is cut from the left so the file name stays visible (direction: rtl); the
		// left-to-right marks keep slashes and the colon where they belong.
		previewTitle.textContent = `\u200e${item.l === undefined ? item.d : `${item.d}:${item.l}`}\u200e`;
		if (key === previewKey) {
			return;
		}
		previewKey = key;
		const cached = previews.get(key);
		if (cached) {
			showPreview(cached);
			return;
		}
		previewTimer = setTimeout(() => vscode.postMessage({ type: 'preview', path: item.p, line: item.l }), PREVIEW_DELAY);
	}

	function showPreview(msg) {
		if (msg.binary) {
			previewBody.innerHTML = '<div class="placeholder">Binary file</div>';
			return;
		}
		previewBody.innerHTML = msg.lines.map((line, i) => {
			const n = msg.start + i;
			return `<div class="line${n === msg.line ? ' target' : ''}"><span class="number">${n}</span><span>${line || ' '}</span></div>`;
		}).join('');
		const target = previewBody.querySelector('.target');
		previewBody.scrollTop = target ? target.offsetTop - previewBody.clientHeight / 3 : 0;
	}

	function open(split) {
		const item = itemAt(selected);
		if (item) {
			vscode.postMessage({ type: 'open', item, split });
		}
	}

	function deleteWordBackward() {
		const end = query.selectionStart;
		const before = query.value.slice(0, end).replace(/\S*\s*$/, '');
		query.value = before + query.value.slice(end);
		query.setSelectionRange(before.length, before.length);
		query.dispatchEvent(new Event('input'));
	}

	query.addEventListener('input', () => {
		selected = 0;
		offset = 0;
		vscode.postMessage({ type: 'query', text: query.value });
		requestView(true);
		render();
	});

	// Helix picker keys. stopPropagation keeps every key inside the picker: the webview would
	// otherwise forward it to VS Code, which runs its own bindings (Ctrl+P quick open, Esc, ...).
	query.addEventListener('keydown', e => {
		e.stopPropagation();
		const ctrl = e.ctrlKey && !e.altKey && !e.metaKey;
		const page = Math.max(1, rows - 1);
		if (e.key === 'ArrowDown' || (ctrl && e.key === 'n') || (e.key === 'Tab' && !e.shiftKey)) select(selected + 1);
		else if (e.key === 'ArrowUp' || (ctrl && e.key === 'p') || (e.key === 'Tab' && e.shiftKey)) select(selected - 1);
		else if (e.key === 'PageDown' || (ctrl && e.key === 'd')) select(selected + page);
		else if (e.key === 'PageUp' || (ctrl && e.key === 'u')) select(selected - page);
		else if (e.key === 'Home') select(0);
		else if (e.key === 'End') select(matched - 1);
		else if (e.key === 'Enter') open('none');
		else if (ctrl && e.key === 'v') open('vertical');
		else if (ctrl && e.key === 's') open('horizontal');
		else if (ctrl && e.key === 't') {
			previewWanted = !previewWanted;
			previewKey = null;
			updateLayout();
			requestView(false);
			render();
		} else if (ctrl && e.key === 'w') deleteWordBackward();
		else if (e.key === 'Escape' || (ctrl && e.key === 'c')) vscode.postMessage({ type: 'close' });
		else return;
		e.preventDefault();
	});

	window.addEventListener('message', ({ data: msg }) => {
		switch (msg.type) {
			case 'init':
				kind = msg.kind;
				query.value = msg.query;
				query.placeholder = kind === 'files' ? 'Find file' : 'Search (regex)';
				selected = msg.selected;
				offset = 0;
				items = [];
				matched = total = 0;
				error = null;
				previews.clear();
				previewKey = null;
				requestView(true);
				render();
				query.focus();
				break;
			case 'snap':
				matched = msg.matched;
				total = msg.total;
				running = msg.running;
				error = msg.error;
				items = msg.items;
				itemsOffset = msg.offset;
				if (selected >= matched) {
					selected = Math.max(0, matched - 1);
				}
				requestView(false);
				render();
				break;
			case 'preview': {
				const key = `${msg.path}:${msg.line ?? ''}`;
				previews.set(key, msg);
				if (key === previewKey) {
					showPreview(msg);
				}
				break;
			}
		}
	});

	window.addEventListener('resize', () => {
		updateLayout();
		requestView(false);
		render();
	});
	window.addEventListener('focus', () => query.focus());
	updateLayout();
	query.focus();
	vscode.postMessage({ type: 'ready' });
})();
