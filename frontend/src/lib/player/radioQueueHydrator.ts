/**
 * Resolves upcoming YouTube tracks before playback. Unresolvable tracks are
 * removed; cross-origin previews stay in deckSampler because Web Audio mutes them.
 */
import { api } from '$lib/api/client';
import { DiscoverV3Api } from '$lib/queries/discover/DiscoverV3Api';
import { playerStore } from '$lib/stores/player.svelte';
import { radioSession } from '$lib/stores/radioSession.svelte';
import type { QueueItem } from '$lib/player/types';

const LOOKAHEAD = 3;
const TICK_MS = 2000;

let timer: ReturnType<typeof setInterval> | null = null;
let hydrating = new Set<string>();

export function needsHydration(item: QueueItem): boolean {
	if (!item.playlistTrackId?.startsWith('radio:')) return false;
	return item.sourceType === 'youtube' && !item.trackSourceId;
}

export async function resolveRadioPatch(item: QueueItem): Promise<Partial<QueueItem> | null> {
	if (item.sourceType !== 'youtube' || item.trackSourceId) return {};
	try {
		const data = await api.global.v3.GET(
			DiscoverV3Api.youtubeTrackSearch(item.artistName, item.trackName)
		);
		if (data.video_id) {
			return { trackSourceId: data.video_id };
		}
	} catch {
		return null;
	}
	return null;
}

function removeUnplayable(item: QueueItem): void {
	const index = playerStore.queue.findIndex(
		(q: QueueItem) => q.playlistTrackId === item.playlistTrackId
	);
	if (index >= 0 && index !== playerStore.currentIndex) {
		playerStore.removeFromQueue(index);
	}
}

async function hydrateOne(item: QueueItem): Promise<void> {
	const key = item.playlistTrackId!;
	if (hydrating.has(key)) return;
	hydrating.add(key);
	try {
		const patch = await resolveRadioPatch(item);
		if (patch === null) {
			removeUnplayable(item);
		} else if (Object.keys(patch).length > 0) {
			playerStore.patchQueueItemByPlaylistTrackId(key, patch);
		}
	} finally {
		hydrating.delete(key);
	}
}

async function tick(): Promise<void> {
	if (!radioSession.active) {
		stopRadioHydration();
		return;
	}
	const queue = playerStore.queue as QueueItem[];
	const from = playerStore.currentIndex;
	const window = queue.slice(from, from + 1 + LOOKAHEAD);
	await Promise.all(window.filter(needsHydration).map(hydrateOne));
}

export function prepareRadioHydration(): void {
	stopRadioHydration();
	hydrating = new Set();
}

export function startRadioHydration(): void {
	if (timer) clearInterval(timer);
	void tick();
	timer = setInterval(() => void tick(), TICK_MS);
}

export function stopRadioHydration(): void {
	if (timer) {
		clearInterval(timer);
		timer = null;
	}
}
