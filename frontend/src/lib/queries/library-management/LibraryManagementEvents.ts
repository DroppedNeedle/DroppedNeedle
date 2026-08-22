import { getApiUrl } from '$lib/api/api-utils';
import { API } from '$lib/constants';

import { invalidateLibraryManagementSurfaces } from './LibraryManagementInvalidation';

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

export function createLibraryManagementEvents() {
	let source: EventSource | null = null;
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
		source = new EventSource(getApiUrl(API.library.operationsStream()));
		source.addEventListener('open', refresh);
		source.addEventListener('activity.changed', handleActivity);
	}

	function stop(): void {
		source?.close();
		source = null;
	}

	return { start, stop };
}
