import { describe, expect, it } from 'vitest';
import type { QueueItem } from '$lib/features/player/playback/types';
import { changeItemSource, updateItemByPlaylistTrackId } from './playerPlaybackMethods';

const localItem: QueueItem = {
	trackSourceId: 'file-1',
	trackName: 'Song',
	artistName: 'Artist',
	trackNumber: 1,
	albumId: 'album-1',
	albumName: 'Album',
	coverUrl: null,
	sourceType: 'local',
	format: 'flac',
	bitRate: 940,
	sampleRate: 44100,
	audioChannels: 2,
	availableSources: ['local', 'jellyfin'],
	sourceIds: { local: 'file-1', jellyfin: 'jf-1' },
	sourceFormats: { local: 'flac', jellyfin: 'opus' },
	playlistTrackId: 'pt-1'
};

describe('playerPlaybackMethods source switching', () => {
	it('drops local quality metadata when switching to a remote source and restores it on return', () => {
		const toRemote = changeItemSource([localItem], 0, 'jellyfin');
		expect(toRemote.error).toBeUndefined();
		expect(toRemote.newQueue[0].sourceType).toBe('jellyfin');
		expect(toRemote.newQueue[0].format).toBe('opus');
		expect(toRemote.newQueue[0].bitRate).toBeUndefined();
		expect(toRemote.newQueue[0].sampleRate).toBeUndefined();
		expect(toRemote.newQueue[0].audioChannels).toBeUndefined();

		const backToLocal = changeItemSource(toRemote.newQueue, 0, 'local');
		expect(backToLocal.error).toBeUndefined();
		expect(backToLocal.newQueue[0].sourceType).toBe('local');
		expect(backToLocal.newQueue[0].format).toBe('flac');
		expect(backToLocal.newQueue[0].bitRate).toBe(940);
		expect(backToLocal.newQueue[0].sampleRate).toBe(44100);
		expect(backToLocal.newQueue[0].audioChannels).toBe(2);
	});

	it('clears quality metadata when updateItemByPlaylistTrackId switches a queued track source', () => {
		const updated = updateItemByPlaylistTrackId([localItem], 'pt-1', 1, 'jellyfin', 'jf-9', 'mp3');
		expect(updated).not.toBeNull();
		expect(updated![0].sourceType).toBe('jellyfin');
		expect(updated![0].format).toBe('mp3');
		expect(updated![0].bitRate).toBeUndefined();
		expect(updated![0].sampleRate).toBeUndefined();
		expect(updated![0].audioChannels).toBeUndefined();
	});
});
