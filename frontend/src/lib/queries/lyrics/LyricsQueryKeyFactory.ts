import type { SourceType } from '$lib/player/types';
import { key, userScope } from '../keys';

export const LyricsQueryKeyFactory = {
	feature: 'lyrics',
	lyrics: (
		userId: string | null | undefined,
		navidromeScope: string | undefined,
		sourceType: SourceType | undefined,
		trackSourceId: string | undefined,
		artistName: string | undefined,
		trackName: string | undefined
	) =>
		key(
			userScope(userId),
			'lyrics',
			navidromeScope,
			sourceType,
			trackSourceId,
			artistName,
			trackName
		)
};
