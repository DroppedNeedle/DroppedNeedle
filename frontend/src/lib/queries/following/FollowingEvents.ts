import { API } from '$lib/constants';
import { toastStore } from '$lib/stores/toast';
import { authStore } from '$lib/stores/authStore.svelte';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { PlaylistQueryKeyFactory } from '$lib/queries/playlists/PlaylistQueryKeyFactory';
import { FollowQueryKeyFactory } from '$lib/queries/following/FollowQueryKeyFactory';

// SSEPublisher replays its last payload to every new subscriber, so toasting
// events arrive again on each reconnect. De-dupe them by id, persisted per
// session, so each one toasts at most once.
const SEEN_KEY = 'msr:auto_download_toasts';
const MIX_SEEN_KEY = 'msr:personal_mix_toasts';

function loadSeen(key: string): Set<string> {
	try {
		const raw = sessionStorage.getItem(key);
		return new Set(raw ? (JSON.parse(raw) as string[]) : []);
	} catch {
		return new Set();
	}
}

function persistSeen(key: string, seen: Set<string>): void {
	try {
		const ids = [...seen].slice(-100);
		sessionStorage.setItem(key, JSON.stringify(ids));
	} catch {
		// sessionStorage unavailable - de-dupe stays in-memory only
	}
}

export function createFollowingEvents() {
	let source: EventSource | null = null;
	let seen = new Set<string>();
	// Spotify import completions replay on reconnect too; de-dupe by event_id so the
	// playlist queries are invalidated once per real import (in-memory is enough - a
	// redundant invalidation is idempotent, unlike a repeated toast).
	let importsSeen = new Set<string>();
	// Weekly Mix refresh completions toast, so their event_id de-dupe persists to
	// sessionStorage like auto_download_enqueued - in-memory alone would replay the
	// retained event's toast on every page load.
	let mixSeen = new Set<string>();

	function handlePlaylistImported(event: Event): void {
		let data: Record<string, unknown>;
		try {
			data = JSON.parse((event as MessageEvent).data) as Record<string, unknown>;
		} catch {
			return;
		}
		const playlistId = typeof data.playlist_id === 'string' ? data.playlist_id : '';
		const eventId = typeof data.event_id === 'string' ? data.event_id : '';
		if (!playlistId || (eventId && importsSeen.has(eventId))) return;
		if (eventId) importsSeen.add(eventId);
		// import finished populating - refresh the open detail view and the list count
		const userId = authStore.user?.id;
		void invalidateQueriesWithPersister({
			queryKey: PlaylistQueryKeyFactory.detail(userId, playlistId)
		});
		void invalidateQueriesWithPersister({ queryKey: PlaylistQueryKeyFactory.list(userId) });
	}

	function handlePersonalMixRefreshed(event: Event): void {
		let data: Record<string, unknown>;
		try {
			data = JSON.parse((event as MessageEvent).data) as Record<string, unknown>;
		} catch {
			return;
		}
		const eventId = typeof data.event_id === 'string' ? data.event_id : '';
		if (eventId && mixSeen.has(eventId)) return;
		if (eventId) {
			mixSeen.add(eventId);
			persistSeen(MIX_SEEN_KEY, mixSeen);
		}
		const userId = authStore.user?.id;
		const playlistId = typeof data.playlist_id === 'string' ? data.playlist_id : '';
		if (playlistId) {
			void invalidateQueriesWithPersister({
				queryKey: PlaylistQueryKeyFactory.detail(userId, playlistId)
			});
		}
		void invalidateQueriesWithPersister({ queryKey: PlaylistQueryKeyFactory.list(userId) });
		if (data.skipped === true) {
			const reason = typeof data.reason === 'string' ? data.reason : '';
			toastStore.show({
				message:
					reason === 'no_tracks'
						? "Couldn't build Your Weekly Mix yet - listen to a bit more first."
						: "Couldn't refresh Your Weekly Mix.",
				type: 'info'
			});
			return;
		}
		const trackCount = typeof data.track_count === 'number' ? data.track_count : 0;
		const requested = typeof data.requested_albums === 'number' ? data.requested_albums : 0;
		const requestedNote = requested
			? `, ${requested} ${requested === 1 ? 'album' : 'albums'} requested`
			: '';
		toastStore.show({
			message: `Your Weekly Mix updated: ${trackCount} tracks${requestedNote}.`,
			type: 'success'
		});
	}

	function handleEnqueued(event: Event): void {
		let data: Record<string, unknown>;
		try {
			data = JSON.parse((event as MessageEvent).data) as Record<string, unknown>;
		} catch {
			return;
		}
		const taskId = typeof data.task_id === 'string' ? data.task_id : '';
		if (!taskId || seen.has(taskId)) return;
		seen.add(taskId);
		persistSeen(SEEN_KEY, seen);
		const title = typeof data.title === 'string' && data.title ? data.title : 'a new release';
		toastStore.show({ message: `Auto-downloading new release: ${title}`, type: 'info' });
		// an enqueue means the poller just found something - refresh the sidebar badge
		void invalidateQueriesWithPersister({
			queryKey: FollowQueryKeyFactory.newReleasesUnseen(authStore.user?.id)
		});
	}

	function start(): void {
		stop();
		seen = loadSeen(SEEN_KEY);
		importsSeen = new Set();
		mixSeen = loadSeen(MIX_SEEN_KEY);
		source = new EventSource(API.following.events());
		source.addEventListener('auto_download_enqueued', handleEnqueued);
		source.addEventListener('playlist_imported', handlePlaylistImported);
		source.addEventListener('personal_mix_refreshed', handlePersonalMixRefreshed);
	}

	function stop(): void {
		if (source) {
			source.close();
			source = null;
		}
	}

	return { start, stop };
}
