import { API } from '$lib/constants';
import { toastStore } from '$lib/stores/toast';
import { authStore } from '$lib/stores/authStore.svelte';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { PlaylistQueryKeyFactory } from '$lib/queries/playlists/PlaylistQueryKeyFactory';
import { FollowQueryKeyFactory } from '$lib/queries/following/FollowQueryKeyFactory';
import { WantedQueryKeyFactory } from '$lib/queries/wanted/WantedQueryKeyFactory';
import { DropImportQueryKeyFactory } from '$lib/queries/import/DropImportQueryKeyFactory';

// SSEPublisher replays its last payload to every new subscriber, so toasting
// events arrive again on each reconnect. De-dupe them by id, persisted per
// session, so each one toasts at most once.
const SEEN_KEY = 'msr:auto_download_toasts';
const MIX_SEEN_KEY = 'msr:personal_mix_toasts';
const WANTED_SEEN_KEY = 'msr:wanted_toasts';
const REQUEST_IMPORTED_SEEN_KEY = 'msr:request_imported_toasts';
const PLUGIN_FETCH_SEEN_KEY = 'msr:plugin_fetch_toasts';

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
	// Wanted watcher events toast (auto-dispatch/fulfilled) - same persisted de-dupe.
	let wantedSeen = new Set<string>();
	// A curator drop-imported an album this user requested - toasts, so persisted.
	let requestImportedSeen = new Set<string>();
	// Plugin-source fetch failures toast too, and need their own de-dupe set:
	// sharing one would let either stream evict the other's ids from the cap.
	let pluginFetchSeen = new Set<string>();

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

	function invalidateWantedList(): void {
		void invalidateQueriesWithPersister({
			queryKey: WantedQueryKeyFactory.list(authStore.user?.id)
		});
	}

	// badge-only: the new-candidates count lives in the wanted list data, so a
	// refetch is the whole reaction (idempotent - no de-dupe needed)
	function handleWantedNewCandidates(): void {
		invalidateWantedList();
	}

	// badge-only: the events sweep found concerts for an artist this user
	// follows - refetch the badge count and the list (idempotent, no de-dupe)
	function handleConcertsNew(): void {
		const userId = authStore.user?.id;
		void invalidateQueriesWithPersister({
			queryKey: FollowQueryKeyFactory.concertsUnseen(userId)
		});
		void invalidateQueriesWithPersister({
			queryKey: FollowQueryKeyFactory.concerts(userId)
		});
	}

	function makeWantedToastHandler(message: (album: string) => string) {
		return (event: Event): void => {
			let data: Record<string, unknown>;
			try {
				data = JSON.parse((event as MessageEvent).data) as Record<string, unknown>;
			} catch {
				return;
			}
			invalidateWantedList();
			const eventId = typeof data.event_id === 'string' ? data.event_id : '';
			if (!eventId || wantedSeen.has(eventId)) return;
			wantedSeen.add(eventId);
			persistSeen(WANTED_SEEN_KEY, wantedSeen);
			const album =
				typeof data.album_title === 'string' && data.album_title ? data.album_title : 'an album';
			toastStore.show({ message: message(album), type: 'success' });
		};
	}

	const handleWantedAutoDispatched = makeWantedToastHandler(
		(album) => `Found a copy of ${album} - downloading it now.`
	);
	const handleWantedFulfilled = makeWantedToastHandler(
		(album) => `${album} is now in your library.`
	);

	// A curator imported an album this user had requested (drop importer, 01c).
	function handleRequestImported(event: Event): void {
		let data: Record<string, unknown>;
		try {
			data = JSON.parse((event as MessageEvent).data) as Record<string, unknown>;
		} catch {
			return;
		}
		const eventId = typeof data.event_id === 'string' ? data.event_id : '';
		if (!eventId || requestImportedSeen.has(eventId)) return;
		requestImportedSeen.add(eventId);
		persistSeen(REQUEST_IMPORTED_SEEN_KEY, requestImportedSeen);
		const album =
			typeof data.album_title === 'string' && data.album_title ? data.album_title : 'An album';
		toastStore.show({ message: `${album} is now in your library.`, type: 'success' });
	}

	// The user's own drop-import job progressed server-side - refresh the lists.
	function handleDropImportUpdated(): void {
		void invalidateQueriesWithPersister({ queryKey: DropImportQueryKeyFactory.prefix });
	}

	// A plugin-source fetch this user started died before producing a job.
	function handlePluginFetchFailed(event: Event): void {
		let data: Record<string, unknown>;
		try {
			data = JSON.parse((event as MessageEvent).data) as Record<string, unknown>;
		} catch {
			return;
		}
		const eventId = typeof data.event_id === 'string' ? data.event_id : '';
		if (!eventId || pluginFetchSeen.has(eventId)) return;
		pluginFetchSeen.add(eventId);
		persistSeen(PLUGIN_FETCH_SEEN_KEY, pluginFetchSeen);
		const plugin = typeof data.plugin === 'string' ? data.plugin : 'a plugin';
		const message = typeof data.message === 'string' ? data.message : 'the fetch failed';
		toastStore.show({ message: `${plugin}: ${message}.`, type: 'error' });
	}

	function start(): void {
		stop();
		seen = loadSeen(SEEN_KEY);
		importsSeen = new Set();
		mixSeen = loadSeen(MIX_SEEN_KEY);
		wantedSeen = loadSeen(WANTED_SEEN_KEY);
		requestImportedSeen = loadSeen(REQUEST_IMPORTED_SEEN_KEY);
		pluginFetchSeen = loadSeen(PLUGIN_FETCH_SEEN_KEY);
		source = new EventSource(API.following.events());
		source.addEventListener('auto_download_enqueued', handleEnqueued);
		source.addEventListener('playlist_imported', handlePlaylistImported);
		source.addEventListener('personal_mix_refreshed', handlePersonalMixRefreshed);
		source.addEventListener('wanted_new_candidates', handleWantedNewCandidates);
		source.addEventListener('concerts_new', handleConcertsNew);
		source.addEventListener('wanted_auto_dispatched', handleWantedAutoDispatched);
		source.addEventListener('wanted_fulfilled', handleWantedFulfilled);
		source.addEventListener('request_imported', handleRequestImported);
		source.addEventListener('drop_import_updated', handleDropImportUpdated);
		source.addEventListener('plugin_fetch_failed', handlePluginFetchFailed);
	}

	function stop(): void {
		if (source) {
			source.close();
			source = null;
		}
	}

	return { start, stop };
}
