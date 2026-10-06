import { api, ApiError } from '$lib/api/client';
import { CACHE_TTL } from '$lib/constants';
import type { LyricLine } from '$lib/types';
import type { NowPlaying } from '$lib/player/types';
import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { LibraryV3Api } from '$lib/queries/library/LibraryV3Api';
import { remoteApi } from '$lib/queries/remotes/remoteApi';
import { LyricsQueryKeyFactory } from './LyricsQueryKeyFactory';

export interface LyricsData {
	text: string;
	is_synced: boolean;
	lines: LyricLine[];
}

export async function fetchLyrics(np: NowPlaying, signal: AbortSignal): Promise<LyricsData | null> {
	const id = np.trackSourceId;
	if (!id) return null;
	try {
		if (np.sourceType === 'local') {
			const data = await api.global.v3.GET(LibraryV3Api.lyrics(id), { signal });
			return {
				text: data.text,
				is_synced: data.is_synced,
				lines: data.lines.map((line) => ({
					text: line.text,
					start_seconds: line.start_seconds ?? null
				}))
			};
		}
		if (np.sourceType === 'navidrome' || np.sourceType === 'jellyfin') {
			// Navidrome's classic lyrics need the artist and title as a fallback.
			const params =
				np.sourceType === 'navidrome' ? { artist: np.artistName, title: np.trackName ?? '' } : {};
			const data = await remoteApi.lyrics(np.sourceType, id, params, signal);
			return {
				text: data.text,
				is_synced: data.is_synced,
				lines: data.lines.map((line) => ({
					text: line.text,
					start_seconds: line.start_ms == null ? null : line.start_ms / 1000
				}))
			};
		}
		return null;
	} catch (e) {
		if (e instanceof ApiError && e.status === 404) return null;
		throw e;
	}
}

export const getLyricsQuery = (
	getNowPlaying: Getter<NowPlaying | null>,
	getUserId: Getter<string | undefined>,
	getNavidromeScope: Getter<string | undefined>
) =>
	createQuery(() => {
		const np = getNowPlaying();
		return {
			staleTime: CACHE_TTL.LYRICS,
			gcTime: CACHE_TTL.LYRICS,
			queryKey: LyricsQueryKeyFactory.lyrics(
				getUserId(),
				np?.sourceType === 'navidrome' ? getNavidromeScope() : undefined,
				np?.sourceType,
				np?.trackSourceId,
				np?.artistName,
				np?.trackName
			),
			queryFn: ({ signal }: { signal: AbortSignal }) => fetchLyrics(np!, signal),
			enabled:
				!!np?.trackSourceId &&
				(np.sourceType === 'local' || np.sourceType === 'navidrome' || np.sourceType === 'jellyfin')
		};
	});
