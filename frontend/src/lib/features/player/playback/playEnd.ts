import type { PlayEnd, PlaybackState } from './types';

/** Why a play ending now ended: it reached its end, it failed, or else `otherwise`. */
export function playEndFrom(state: PlaybackState, otherwise: PlayEnd): PlayEnd {
	if (state === 'ended') return 'finished';
	if (state === 'error') return 'failed';
	return otherwise;
}
