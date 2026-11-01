import { browser } from '$app/environment';
import { PAGE_SOURCE_KEYS } from '$lib/constants';
import { registerUserSessionReset } from '$lib/utils/userSessionCleanup';

// The primary source itself is the scrobble-preferences query; this module
// holds only its type and each page's own switch, kept in the browser.
export type MusicSource = 'listenbrainz' | 'lastfm';

export const DEFAULT_SOURCE: MusicSource = 'listenbrainz';

export function isMusicSource(value: unknown): value is MusicSource {
	return value === 'listenbrainz' || value === 'lastfm';
}

// A shared browser never carries one person's per-page choices to the next.
registerUserSessionReset(() => {
	if (!browser) return;
	for (const key of Object.values(PAGE_SOURCE_KEYS)) localStorage.removeItem(key);
});
