import type { AlbumTracksInfo } from '$lib/types';
import { getDiscTrackKey, normalizeDiscNumber, compareDiscTrack } from '$lib/player/queueHelpers';

export type RenderedTrackSection = {
	discNumber: number;
	items: Array<{ track: AlbumTracksInfo['tracks'][number]; globalIndex: number }>;
};

export function buildSortedTrackMap<
	T extends { track_number?: number | null; disc_number?: number | null }
>(tracks: T[]): Map<string, T> {
	return new Map(
		[...tracks]
			.sort(compareDiscTrack)
			.filter((t) => Number.isFinite(Number(t.track_number)) && Number(t.track_number) > 0)
			.map((t) => [getDiscTrackKey(t), t] as const)
	);
}

export function resolveSourceTrack<
	T extends { track_number?: number | null; disc_number?: number | null }
>(
	discNumber: number | undefined,
	position: number,
	rowIndex: number,
	trackMap: Map<string, T>,
	tracks: T[]
): T | null {
	const trackKey = getDiscTrackKey({ disc_number: discNumber, track_number: position });
	if (!trackKey.endsWith(':0')) {
		const byNumber = trackMap.get(trackKey);
		if (byNumber) return byNumber;
	}

	const numberingIsUnusable = tracks.length > 0 && trackMap.size < tracks.length;
	if (numberingIsUnusable && rowIndex >= 0 && rowIndex < tracks.length) {
		return tracks[rowIndex] ?? null;
	}

	return null;
}

export function buildRenderedTrackSections(
	tracks: AlbumTracksInfo['tracks']
): RenderedTrackSection[] {
	const grouped = new Map<
		number,
		Array<{ track: AlbumTracksInfo['tracks'][number]; globalIndex: number }>
	>();
	tracks.forEach((track) => {
		const discNumber = normalizeDiscNumber(track.disc_number);
		const entry = { track, globalIndex: 0 };
		const existing = grouped.get(discNumber);
		if (existing) {
			existing.push(entry);
		} else {
			grouped.set(discNumber, [entry]);
		}
	});
	const sections = Array.from(grouped.entries())
		.sort(([a], [b]) => a - b)
		.map(([discNumber, items]) => ({
			discNumber,
			items: items.sort((a, b) => Number(a.track.position) - Number(b.track.position))
		}));
	let sortedIdx = 0;
	for (const section of sections) {
		for (const item of section.items) {
			item.globalIndex = sortedIdx++;
		}
	}
	return sections;
}
