import type { NowPlayingSession } from '$lib/types';

/**
 * Combine the viewer's own local web-player session with the server presence feed.
 *
 * Only the web player's *own* echo (`{myId}:web`) is dropped - it's already shown,
 * un-redacted and instant, via the local overlay, so we'd otherwise see it twice.
 * The viewer's OTHER devices (connected apps like Manet/Finamp, keyed
 * `{myId}:compat:*`) and everyone else are kept. An upstream-server poll echo of the
 * very track playing locally is also de-duped.
 */
export function mergeNowPlayingSessions(
	local: NowPlayingSession | null,
	server: NowPlayingSession[],
	myId: string | undefined
): NowPlayingSession[] {
	const webEchoId = myId ? `${myId}:web` : null;
	let filtered = webEchoId ? server.filter((s) => s.id !== webEchoId) : server.slice();
	if (local) {
		filtered = filtered.filter(
			(s) => !(s.source === local.source && s.track_name === local.track_name)
		);
		return [local, ...filtered];
	}
	return filtered;
}
