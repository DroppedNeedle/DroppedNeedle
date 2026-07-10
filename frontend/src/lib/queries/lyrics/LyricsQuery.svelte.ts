import { api, ApiError } from '$lib/api/client';
import { API, CACHE_TTL } from '$lib/constants';
import type { LyricLine, JellyfinLyricsResponse, NavidromeLyricsResponse } from '$lib/types';
import type { NowPlaying } from '$lib/player/types';
import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { LyricsQueryKeyFactory } from './LyricsQueryKeyFactory';

export interface LyricsData {
	text: string;
	is_synced: boolean;
	lines: LyricLine[];
}

export async function fetchLyrics(np: NowPlaying, signal: AbortSignal): Promise<LyricsData | null> {
	try {
		if (np.sourceType === 'navidrome') {
			const url = API.navidromeLibrary.lyrics(np.trackSourceId!, np.artistName, np.trackName ?? '');
			const data = await api.global.get<NavidromeLyricsResponse>(url, { signal });
			return {
				text: data.text ?? '',
				is_synced: data.is_synced ?? false,
				lines: data.lines ?? []
			};
		}
		if (np.sourceType === 'jellyfin') {
			const url = API.jellyfinLibrary.lyrics(np.trackSourceId!);
			const data = await api.global.get<JellyfinLyricsResponse>(url, { signal });
			return {
				text: data.lyrics_text ?? '',
				is_synced: data.is_synced ?? false,
				lines: data.lines ?? []
			};
		}
		if (np.sourceType === 'local') {
			// DroppedNeedle's own library: embedded tags + sidecar .lrc, read off disk
			const url = API.library.trackLyrics(np.trackSourceId!);
			const data = await api.global.get<LyricsData>(url, { signal });
			return {
				text: data.text ?? '',
				is_synced: data.is_synced ?? false,
				lines: data.lines ?? []
			};
		}
		if (np.sourceType === 'plex') {
			// Plex streams have no DroppedNeedle library file: metadata-only LRCLIB
			// lookup, gated server-side on the lyrics fetch setting (off -> 404 -> null)
			if (!np.trackName) return null;
			const url = API.lyrics.lookup(np.artistName, np.trackName, np.albumName || undefined);
			const data = await api.global.get<LyricsData>(url, { signal });
			return {
				text: data.text ?? '',
				is_synced: data.is_synced ?? false,
				lines: data.lines ?? []
			};
		}
		return null;
	} catch (e) {
		if (e instanceof ApiError && e.status === 404) return null;
		throw e;
	}
}

export const getLyricsQuery = (getNowPlaying: Getter<NowPlaying | null>) =>
	createQuery(() => {
		const np = getNowPlaying();
		return {
			staleTime: CACHE_TTL.LYRICS,
			gcTime: CACHE_TTL.LYRICS,
			queryKey: LyricsQueryKeyFactory.lyrics(
				np?.sourceType,
				np?.trackSourceId,
				np?.artistName,
				np?.trackName
			),
			queryFn: ({ signal }: { signal: AbortSignal }) => fetchLyrics(np!, signal),
			enabled:
				!!np?.trackSourceId &&
				(np.sourceType === 'navidrome' ||
					np.sourceType === 'jellyfin' ||
					np.sourceType === 'local' ||
					np.sourceType === 'plex')
		};
	});
