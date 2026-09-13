import { invalidateLibraryManagementSurfaces } from './LibraryManagementInvalidation';
import {
	muxEventStream,
	type MuxEventStream,
	type MuxUnsubscribe
} from '$lib/queries/events/MuxEventStream';

export interface LibraryManagementActivityEvent {
	id: string;
	revisions: Record<string, number>;
}

const MAX_SEEN_EVENT_IDS = 100;

export function parseLibraryManagementActivityEvent(
	data: unknown
): LibraryManagementActivityEvent | null {
	if (typeof data !== 'string') return null;
	let value: unknown;
	try {
		value = JSON.parse(data);
	} catch {
		return null;
	}
	if (typeof value !== 'object' || value === null || Array.isArray(value)) return null;
	const record = value as Record<string, unknown>;
	if (typeof record.id !== 'string' || record.id.length === 0) return null;
	if (
		typeof record.revisions !== 'object' ||
		record.revisions === null ||
		Array.isArray(record.revisions)
	)
		return null;
	const revisions: Record<string, number> = {};
	for (const [key, revision] of Object.entries(record.revisions)) {
		if (typeof revision !== 'number' || !Number.isSafeInteger(revision) || revision < 0) {
			return null;
		}
		revisions[key] = revision;
	}
	return { id: record.id, revisions };
}

export function createLibraryManagementEvents(mux: MuxEventStream = muxEventStream) {
	let unsubs: MuxUnsubscribe[] = [];
	const seenIds = new Set<string>();
	const seenOrder: string[] = [];

	function remember(eventId: string): boolean {
		if (seenIds.has(eventId)) return false;
		seenIds.add(eventId);
		seenOrder.push(eventId);
		while (seenOrder.length > MAX_SEEN_EVENT_IDS) {
			const expired = seenOrder.shift();
			if (expired !== undefined) seenIds.delete(expired);
		}
		return true;
	}

	function refresh(): void {
		void invalidateLibraryManagementSurfaces();
	}

	function handleActivity(event: Event): void {
		const message = event as MessageEvent<unknown>;
		const parsed = parseLibraryManagementActivityEvent(message.data);
		if (parsed === null) return;
		const eventId = message.lastEventId || parsed.id;
		if (!remember(eventId)) return;
		refresh();
	}

	function start(): void {
		stop();
		unsubs = [mux.on('activity.changed', handleActivity), mux.onConnect(refresh)];
		// Refresh directly only when already connected; otherwise the imminent
		// first open fires refresh and a direct call would double it.
		if (mux.isConnected) refresh();
	}

	function stop(): void {
		for (const unsub of unsubs) unsub();
		unsubs = [];
	}

	return { start, stop };
}
