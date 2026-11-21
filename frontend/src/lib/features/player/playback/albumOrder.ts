import type { QueueItem } from './types';

/**
 * Whether `next` follows `previous` on the same album: the same album id,
 * and the next track on the same disc or track 1 of the next disc. A
 * missing disc counts as disc 1, and two neighbours from one album with a
 * track number unknown count as following, since they most likely do.
 */
export function followsOnAlbum(previous: QueueItem, next: QueueItem): boolean {
	if (!previous.albumId || previous.albumId !== next.albumId) return false;
	if (!(previous.trackNumber > 0) || !(next.trackNumber > 0)) return true;
	const disc = previous.discNumber || 1;
	const nextDisc = next.discNumber || 1;
	if (nextDisc === disc) return next.trackNumber === previous.trackNumber + 1;
	return nextDisc === disc + 1 && next.trackNumber === 1;
}
