import { api } from '$lib/api/client';

export const COVER_VISUAL_SETTLE_MS = 6500;

const RETRY_DELAYS_MS = [1500, 3000, 5000, 8000, 12000, 20000] as const;
const REQUEST_TIMEOUT_MS = 10_000;

export type CoverWarmUpdate =
	{ status: 'warming' } | { status: 'ready'; url: string } | { status: 'failed' };

type Listener = (update: CoverWarmUpdate) => void;

interface WarmEntry {
	url: string;
	listeners: Set<Listener>;
	retryIndex: number;
	state: CoverWarmUpdate;
	timer: ReturnType<typeof setTimeout> | null;
	controller: AbortController | null;
}

const entries = new Map<string, WarmEntry>();

function retryUrl(url: string, generation: number): string {
	return `${url}${url.includes('?') ? '&' : '?'}_r=${generation}`;
}

function notify(entry: WarmEntry, update: CoverWarmUpdate) {
	entry.state = update;
	for (const listener of entry.listeners) listener(update);
}

function dispose(key: string, entry: WarmEntry) {
	if (entry.timer) clearTimeout(entry.timer);
	entry.controller?.abort();
	if (entry.state.status === 'ready') URL.revokeObjectURL(entry.state.url);
	entries.delete(key);
}

function schedule(key: string, entry: WarmEntry) {
	if (entry.retryIndex >= RETRY_DELAYS_MS.length) {
		notify(entry, { status: 'failed' });
		return;
	}

	const delay = RETRY_DELAYS_MS[entry.retryIndex];
	entry.timer = setTimeout(() => void poll(key, entry), delay);
}

async function poll(key: string, entry: WarmEntry) {
	entry.timer = null;
	if (entries.get(key) !== entry || entry.listeners.size === 0) return;

	const generation = ++entry.retryIndex;
	entry.controller = new AbortController();

	try {
		const response = await api.global.get<Response>(retryUrl(entry.url, generation), {
			raw: true,
			cache: 'no-store',
			signal: entry.controller.signal,
			timeoutMs: REQUEST_TIMEOUT_MS
		});

		if (response.status === 202) {
			schedule(key, entry);
			return;
		}

		if (response.ok && response.headers.get('x-cover-source') !== 'placeholder') {
			const objectUrl = URL.createObjectURL(await response.blob());
			notify(entry, { status: 'ready', url: objectUrl });
			return;
		}

		if (response.status >= 500 && entry.retryIndex < RETRY_DELAYS_MS.length) {
			schedule(key, entry);
			return;
		}

		notify(entry, { status: 'failed' });
	} catch {
		if (entries.get(key) !== entry || entry.controller.signal.aborted) return;
		if (entry.retryIndex < RETRY_DELAYS_MS.length) schedule(key, entry);
		else notify(entry, { status: 'failed' });
	} finally {
		entry.controller = null;
	}
}

export function watchWarmingCover(url: string, listener: Listener): () => void {
	let entry = entries.get(url);
	if (!entry) {
		entry = {
			url,
			listeners: new Set(),
			retryIndex: 0,
			state: { status: 'warming' },
			timer: null,
			controller: null
		};
		entries.set(url, entry);
		schedule(url, entry);
	}

	entry.listeners.add(listener);
	listener(entry.state);

	return () => {
		entry.listeners.delete(listener);
		if (entry.listeners.size === 0) dispose(url, entry);
	};
}
