import { getApiUrl } from '$lib/api/api-utils';
import { API } from '$lib/constants';

export type MuxEventListener = (event: Event) => void;
export type MuxConnectListener = () => void;
export type MuxUnsubscribe = () => void;

export interface MuxEventStream {
	connect: () => void;
	disconnect: () => void;
	on: (eventName: string, listener: MuxEventListener) => MuxUnsubscribe;
	onConnect: (listener: MuxConnectListener) => MuxUnsubscribe;
	readonly isConnected: boolean;
}

/**
 * The tab's single multiplexed SSE connection (`GET /api/v1/events/stream`).
 * Consumers register named-event listeners; the connection itself is owned by
 * the app shell (connect on login, disconnect on logout) and hibernates while
 * the tab is hidden. Drops reconnect natively, paced by the server-sent
 * `retry:` frame, so no client retry timers are needed.
 */
export function createMuxEventStream(streamUrl: string = API.events.stream()): MuxEventStream {
	let source: EventSource | null = null;
	let active = false;
	const listeners = new Map<string, Set<MuxEventListener>>();
	const connectListeners = new Set<MuxConnectListener>();

	function notifyConnect(): void {
		for (const listener of [...connectListeners]) listener();
	}

	function attachAll(target: EventSource): void {
		for (const [name, set] of listeners) {
			for (const listener of set) target.addEventListener(name, listener);
		}
	}

	function closeSource(): void {
		source?.close();
		source = null;
	}

	function open(): void {
		if (!active) return;
		if (typeof document !== 'undefined' && document.hidden) return;
		closeSource();
		const next = new EventSource(getApiUrl(streamUrl));
		next.onopen = () => notifyConnect();
		attachAll(next);
		source = next;
	}

	function handleVisibilityChange(): void {
		if (!active) return;
		if (document.hidden) closeSource();
		else open();
	}

	return {
		connect(): void {
			if (active) return;
			active = true;
			open();
			if (typeof document !== 'undefined') {
				document.addEventListener('visibilitychange', handleVisibilityChange);
			}
		},

		disconnect(): void {
			active = false;
			closeSource();
			if (typeof document !== 'undefined') {
				document.removeEventListener('visibilitychange', handleVisibilityChange);
			}
		},

		on(eventName: string, listener: MuxEventListener): MuxUnsubscribe {
			let set = listeners.get(eventName);
			if (!set) {
				set = new Set();
				listeners.set(eventName, set);
			}
			set.add(listener);
			source?.addEventListener(eventName, listener);
			return () => {
				const live = listeners.get(eventName);
				live?.delete(listener);
				if (live?.size === 0) listeners.delete(eventName);
				source?.removeEventListener(eventName, listener);
			};
		},

		onConnect(listener: MuxConnectListener): MuxUnsubscribe {
			connectListeners.add(listener);
			return () => {
				connectListeners.delete(listener);
			};
		},

		get isConnected(): boolean {
			return source !== null;
		}
	};
}

export const muxEventStream = createMuxEventStream();
