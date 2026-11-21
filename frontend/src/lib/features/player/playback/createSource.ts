import type { PlaybackSource, SourceType } from './types';
import type { StreamRefusal } from './streamRefusal';
import { YouTubePlaybackSource } from './YouTubePlaybackSource';
import { NativeAudioSource, type LiveTranscode } from './NativeAudioSource';
import { claimDeck } from './audioElement';
import { YOUTUBE_PLAYER_ELEMENT_ID } from '$lib/constants';

export type NativeSourceOptions = {
	url: string;
	seekable: boolean;
	/** Set when the url is a live transcode, which cannot seek or report a length itself. */
	transcode?: LiveTranscode;
	/** Asks the server why it refused the address the element failed on. */
	whyRefused?: (url: string) => Promise<StreamRefusal | null>;
};

export function createPlaybackSource(type: SourceType, opts?: NativeSourceOptions): PlaybackSource {
	switch (type) {
		case 'youtube':
			return new YouTubePlaybackSource(YOUTUBE_PLAYER_ELEMENT_ID);
		case 'jellyfin':
		case 'local':
		case 'navidrome':
		case 'plex':
			if (!opts) throw new Error(`A ${type} playback source requires url and seekable options`);
			// A native source plays on a free deck, which it holds until destroyed.
			return new NativeAudioSource(type, { ...opts, deck: claimDeck() });
		default: {
			const _exhaustive: never = type;
			throw new Error(`Unknown source type: ${_exhaustive}`);
		}
	}
}
