import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync } from 'svelte';
import type { DiscoverActivity } from '$lib/types';

const h = vi.hoisted(() => ({
	post: vi.fn(),
	user: { id: 'user-a' },
	source: { source_mode: 'brainzmash', source_id: 'source-a', generation: 1 },
	setSource: vi.fn()
}));
vi.mock('$lib/api/client', () => ({ api: { global: { v3: { POST: h.post } } } }));
vi.mock('$lib/stores/authStore.svelte', () => ({ authStore: { user: h.user } }));
vi.mock('$lib/queries/musicbrainz/sourceScope.svelte', () => ({
	musicBrainzSourceKey: () => ({ user_id: h.user.id, ...h.source }),
	setMusicBrainzSourceScope: h.setSource
}));
vi.mock('@tanstack/svelte-query', () => ({
	createMutation: (factory: () => { mutationFn: (value: DiscoverActivity) => Promise<void> }) => ({
		mutate: (value: DiscoverActivity) => {
			void factory().mutationFn(value);
		}
	})
}));

import { recordDiscoverActivity, useDiscoverActivity } from './DiscoverDemand.svelte';

let dispose: (() => void) | undefined;
let visibility: DocumentVisibilityState;
let intersection: ((entries: { isIntersecting: boolean }[]) => void) | undefined;

beforeEach(() => {
	vi.clearAllMocks();
	h.user.id = 'user-a';
	h.source.source_id = 'source-a';
	h.source.generation = 1;
	h.post.mockResolvedValue({ source_mode: 'brainzmash', source_id: 'source-b', generation: 2 });
	visibility = 'visible';
	vi.spyOn(document, 'visibilityState', 'get').mockImplementation(() => visibility);
	vi.stubGlobal(
		'IntersectionObserver',
		class {
			constructor(callback: typeof intersection) {
				intersection = callback;
			}
			observe() {}
			disconnect() {}
		}
	);
});
afterEach(() => {
	dispose?.();
	dispose = undefined;
	vi.restoreAllMocks();
	vi.unstubAllGlobals();
});

describe('visible discovery demand', () => {
	it('records Home entry without a data GET and revalidates source on focus', async () => {
		dispose = $effect.root(() => useDiscoverActivity(() => ({ feature: 'home' })));
		flushSync();
		await vi.waitFor(() =>
			expect(h.setSource).toHaveBeenCalledWith(
				{ source_mode: 'brainzmash', source_id: 'source-b', generation: 2 },
				'user-a'
			)
		);
		h.post.mockClear();
		window.dispatchEvent(new Event('focus'));
		expect(h.post).toHaveBeenCalledWith(
			'/api/v3/discover/activity',
			{ feature: 'home' },
			{ signal: undefined }
		);
	});

	it('does not record a hidden or offscreen artist section, then records its selected provider on entry', () => {
		visibility = 'hidden';
		const element = document.createElement('section');
		dispose = $effect.root(() =>
			useDiscoverActivity(
				() => ({
					feature: 'artist',
					artist_mbid: 'artist-a',
					section: 'top_songs',
					provider: 'lastfm'
				}),
				() => element
			)
		);
		flushSync();
		intersection?.([{ isIntersecting: true }]);
		expect(h.post).not.toHaveBeenCalled();
		visibility = 'visible';
		document.dispatchEvent(new Event('visibilitychange'));
		expect(h.post).toHaveBeenCalledWith(
			'/api/v3/discover/activity',
			{
				feature: 'artist',
				artist_mbid: 'artist-a',
				section: 'top_songs',
				provider: 'lastfm'
			},
			{ signal: undefined }
		);
	});

	it('does not restore a late source response after an account switch', async () => {
		const pending = Promise.withResolvers<unknown>();
		h.post.mockReturnValue(pending.promise);
		const recording = recordDiscoverActivity({ feature: 'queue' });
		h.user.id = 'user-b';
		pending.resolve({ source_mode: 'brainzmash', source_id: 'source-a', generation: 1 });
		await recording;
		expect(h.setSource).not.toHaveBeenCalled();
	});

	it('does not revive an earlier source generation after a newer source was observed', async () => {
		const pending = Promise.withResolvers<unknown>();
		h.post.mockReturnValue(pending.promise);
		const recording = recordDiscoverActivity({ feature: 'queue' });
		h.source.source_id = 'source-c';
		h.source.generation = 3;
		pending.resolve({ source_mode: 'brainzmash', source_id: 'source-b', generation: 2 });
		await recording;
		expect(h.setSource).not.toHaveBeenCalled();
	});
});
