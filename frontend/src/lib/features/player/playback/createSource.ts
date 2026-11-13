import type { PlaybackSource, SourceType } from './types';
import type { StreamRefusal } from './streamRefusal';
import { YouTubePlaybackSource } from './YouTubePlaybackSource';
import { NativeAudioSource, type LiveTranscode } from './NativeAudioSource';
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
			if (!opts) throw new Error('Jellyfin playback source requires url and seekable options');
			return new NativeAudioSource('jellyfin', opts);
		case 'local':
			if (!opts) throw new Error('Local playback source requires url and seekable options');
			return new NativeAudioSource('local', opts);
		case 'navidrome':
			if (!opts) throw new Error('Navidrome playback source requires url and seekable options');
			return new NativeAudioSource('navidrome', opts);
		case 'plex':
			if (!opts) throw new Error('Plex playback source requires url and seekable options');
			return new NativeAudioSource('plex', opts);
		default: {
			const _exhaustive: never = type;
			throw new Error(`Unknown source type: ${_exhaustive}`);
		}
	}
}
