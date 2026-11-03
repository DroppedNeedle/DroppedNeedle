import { beforeEach, expect, it, vi } from 'vitest';

import type { QueueItem } from '$lib/features/player/playback/types';
import {
	openGlobalPlaylistModal,
	playlistModalState,
	registerPlaylistModal,
	resetPlaylistModal,
	unregisterPlaylistModal
} from './playlistModal.svelte';

const track: QueueItem = {
	trackSourceId: 'track-1',
	trackName: 'Track',
	artistName: 'Artist',
	trackNumber: 1,
	albumId: 'album-1',
	albumName: 'Album',
	coverUrl: null,
	sourceType: 'local'
};

beforeEach(() => resetPlaylistModal());

it('retains the first-open request until the lazy modal registers', () => {
	const firstOpen = vi.fn();
	const secondOpen = vi.fn();

	expect(playlistModalState.shouldMount).toBe(false);
	openGlobalPlaylistModal([track]);
	expect(playlistModalState.shouldMount).toBe(true);
	expect(firstOpen).not.toHaveBeenCalled();

	const firstHandle = { open: firstOpen };
	registerPlaylistModal(firstHandle);
	expect(firstOpen).toHaveBeenCalledOnce();
	expect(firstOpen).toHaveBeenCalledWith([track]);

	openGlobalPlaylistModal([track, track]);
	expect(firstOpen).toHaveBeenLastCalledWith([track, track]);

	unregisterPlaylistModal(firstHandle);
	openGlobalPlaylistModal([track]);
	expect(firstOpen).toHaveBeenCalledTimes(2);
	registerPlaylistModal({ open: secondOpen });
	expect(secondOpen).toHaveBeenCalledWith([track]);
});

it('drops a queued open when the authenticated session is reset', () => {
	const nextAccountOpen = vi.fn();

	openGlobalPlaylistModal([track]);
	resetPlaylistModal();
	registerPlaylistModal({ open: nextAccountOpen });

	expect(playlistModalState.shouldMount).toBe(false);
	expect(nextAccountOpen).not.toHaveBeenCalled();
});
