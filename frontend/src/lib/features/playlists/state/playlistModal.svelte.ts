import type { QueueItem } from '$lib/features/player/playback/types';

export type PlaylistModalHandle = { open: (items: QueueItem[]) => void };

let shouldMount = $state(false);
let instance: PlaylistModalHandle | null = null;
let pendingItems: QueueItem[] | null = null;

function flushPending(): void {
	if (!instance || !pendingItems) return;
	const items = pendingItems;
	pendingItems = null;
	instance.open(items);
}

export const playlistModalState = {
	get shouldMount() {
		return shouldMount;
	}
};

export function registerPlaylistModal(ref: PlaylistModalHandle): void {
	instance = ref;
	flushPending();
}

export function unregisterPlaylistModal(ref?: PlaylistModalHandle): void {
	if (!ref || instance === ref) instance = null;
}

export function resetPlaylistModal(): void {
	instance = null;
	pendingItems = null;
	shouldMount = false;
}

export function openGlobalPlaylistModal(items: QueueItem[]): void {
	pendingItems = items;
	shouldMount = true;
	flushPending();
}
