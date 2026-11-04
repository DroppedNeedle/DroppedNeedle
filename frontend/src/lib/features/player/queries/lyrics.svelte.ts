import { LyricsQueryKeyFactory } from '$lib/queries/featureKeys';
import type { NowPlaying } from '$lib/features/player/playback/types';

import { remoteQuery } from '$lib/queries/remotes/params';
import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import { ApiError } from '$lib/api/errors';
import { STALE_SLOW } from '$lib/queries/staleTime';
import type { LyricLine } from '$lib/features/player/queries/types';
import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

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
			const data = await api.v3.GET(
				v3('/api/v3/library/tracks/{id}/lyrics', { path: { id: id } }),
				{ signal }
			);
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
			const data = await api.v3.GET(
				v3('/api/v3/remotes/{source}/lyrics/{id}', {
					path: { source: np.sourceType, id },
					query: remoteQuery(params)
				}),
				{ signal }
			);
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
			staleTime: STALE_SLOW,
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
