import type { components } from '$lib/api/v3/openapi';
import { queryClient } from '$lib/queries/QueryClient';
import {
	applyEffects,
	catalogChanged,
	concertsChanged,
	downloadsChanged,
	dropImportsChanged,
	libraryActivityChanged,
	libraryOperationsChanged,
	localChanged,
	newReleasesChanged,
	playlistsChanged,
	requestsChanged,
	searchJobChanged,
	searchJobsChanged,
	wantedChanged,
	type Effect
} from '$lib/queries/effects';
import { LibraryQueryKeyFactory } from '$lib/queries/library/LibraryQueryKeyFactory';
import type { LibraryActivityResponse } from '$lib/queries/library/LibraryOperationsTypes';
import { toastStore } from '$lib/stores/toast';
import { muxEventStream, type MuxEventStream, type MuxUnsubscribe } from './MuxEventStream';

type StreamEvent = components['schemas']['StreamEvent'];

/** The names the server's event stream sends, from the API document. */
export type ServerEventName = StreamEvent['event'];
export type ServerEventData<N extends ServerEventName> = Extract<StreamEvent, { event: N }>['data'];

export interface EventSession {
	userId: string;
	isAdmin: boolean;
}

export interface EventToast {
	/** Shown once per id, even when the stream replays the event. */
	id: string;
	message: string;
	type: 'success' | 'info';
}

interface EventContext {
	session: EventSession;
	activity: ActivityTracker;
}

export interface EventEntry<N extends ServerEventName> {
	/** What the event says changed. */
	effects?: (data: ServerEventData<N>, session: EventSession) => Effect[];
	/** Work beyond refreshing queries. */
	handle?: (data: ServerEventData<N>, context: EventContext) => void;
	toast?: (data: ServerEventData<N>) => EventToast | null;
}

const albumOr = (title: string | undefined, fallback: string) => title || fallback;

/**
 * Every server event and what the app does with it. Entries with nothing
 * here feed live views only, through `onServerEvent`: download progress,
 * search jobs, cache sync and now-playing.
 */
export const EVENT_TABLE: { [N in ServerEventName]: EventEntry<N> } = {
	'activity.changed': {
		handle: (data, { activity }) => activity.receive(data)
	},
	snapshot: {},
	'downloads.changed': {
		effects: (_, { userId }) => [downloadsChanged(userId), requestsChanged(userId)]
	},
	'cache.sync': {},
	wanted_new_candidates: {
		effects: (_, { userId }) => [wantedChanged(userId)]
	},
	wanted_auto_dispatched: {
		effects: (_, { userId }) => [wantedChanged(userId)],
		toast: (data) => ({
			id: `wanted:${data.event_id}`,
			message: `Found a copy of ${albumOr(data.album_title, 'an album')} - downloading it now.`,
			type: 'success'
		})
	},
	wanted_fulfilled: {
		effects: (_, { userId }) => [wantedChanged(userId)],
		toast: (data) => ({
			id: `wanted:${data.event_id}`,
			message: `${albumOr(data.album_title, 'an album')} is now in your library.`,
			type: 'success'
		})
	},
	auto_download_enqueued: {
		effects: (_, { userId }) => [newReleasesChanged(userId)],
		toast: (data) => ({
			id: `auto-download:${data.task_id}`,
			message: `Auto-downloading new release: ${albumOr(data.title, 'a new release')}`,
			type: 'info'
		})
	},
	request_imported: {
		toast: (data) => ({
			id: `request-imported:${data.event_id}`,
			message: `${albumOr(data.album_title, 'An album')} is now in your library.`,
			type: 'success'
		})
	},
	playlist_imported: {
		effects: (_, { userId }) => [playlistsChanged(userId)]
	},
	drop_import_updated: {
		effects: (_, { userId }) => [dropImportsChanged(userId)]
	},
	personal_mix_refreshed: {
		effects: (_, { userId }) => [playlistsChanged(userId)],
		toast: (data) => ({
			id: `personal-mix:${data.event_id}`,
			message: mixMessage(data),
			type: data.skipped ? 'info' : 'success'
		})
	},
	concerts_new: {
		effects: (_, { userId }) => [concertsChanged(userId)]
	},
	download_progress: {},
	search_job_updated: {
		effects: (data, { userId }) => [searchJobChanged(userId, data.job_id)]
	}
};

function mixMessage(data: ServerEventData<'personal_mix_refreshed'>): string {
	if (data.skipped) {
		return data.reason === 'no_tracks'
			? "Couldn't build Your Weekly Mix yet - listen to a bit more first."
			: "Couldn't refresh Your Weekly Mix.";
	}
	const requested = data.requested_albums
		? `, ${data.requested_albums} ${data.requested_albums === 1 ? 'album' : 'albums'} requested`
		: '';
	return `Your Weekly Mix updated: ${data.track_count} tracks${requested}.`;
}

/**
 * What a (re)connect refreshes. A hidden tab closes the stream, so notices
 * sent meanwhile never arrive; state events replay on their own.
 */
function reconnectEffects({ userId, isAdmin }: EventSession): Effect[] {
	const effects = [
		newReleasesChanged(userId),
		concertsChanged(userId),
		wantedChanged(userId),
		playlistsChanged(userId),
		dropImportsChanged(userId),
		searchJobsChanged(userId),
		downloadsChanged(userId),
		requestsChanged(userId),
		localChanged()
	];
	if (isAdmin) effects.push(libraryOperationsChanged());
	return effects;
}

// The stream replays recent notices on every reconnect, so a toast is shown
// once per id, remembered for the browser session.
const SEEN_TOASTS_KEY = 'dn:event_toasts';
const MAX_SEEN_TOASTS = 200;

function loadSeenToasts(): string[] {
	try {
		const raw = sessionStorage.getItem(SEEN_TOASTS_KEY);
		return raw ? (JSON.parse(raw) as string[]) : [];
	} catch {
		return [];
	}
}

function saveSeenToasts(ids: string[]): void {
	try {
		sessionStorage.setItem(SEEN_TOASTS_KEY, JSON.stringify(ids));
	} catch {
		// no session storage: the de-dupe stays in memory
	}
}

const CATALOG_REFRESH_WINDOW_MS = 10_000;

interface ActivityTracker {
	receive: (data: ServerEventData<'activity.changed'>) => void;
	reset: () => void;
}

/**
 * Turns `activity.changed` revision sets into refreshes: the catalog when its
 * revision moved (at once, then at most once per window during a scan),
 * otherwise library activity, and the admin's runs and reviews.
 */
function createActivityTracker(getSession: () => EventSession | null): ActivityTracker {
	let revisions: Record<string, number> | null = null;
	let pendingInitial: Record<string, number> | null = null;
	let unsubscribeCache: (() => void) | null = null;
	let catalogTimer: ReturnType<typeof setTimeout> | null = null;
	let catalogPending = false;

	function refreshCatalog(): void {
		if (catalogTimer !== null) {
			catalogPending = true;
			return;
		}
		catalogTimer = setTimeout(endCatalogWindow, CATALOG_REFRESH_WINDOW_MS);
		void applyEffects(catalogChanged(getSession()?.userId));
	}

	function endCatalogWindow(): void {
		catalogTimer = null;
		if (!catalogPending) return;
		catalogPending = false;
		refreshCatalog();
	}

	function signature(set: Record<string, number>): string {
		return JSON.stringify(Object.entries(set).sort(([a], [b]) => a.localeCompare(b)));
	}

	function apply(previous: Record<string, number>, next: Record<string, number>): void {
		if (signature(previous) === signature(next)) return;
		if (next.catalog !== previous.catalog) {
			refreshCatalog();
			return;
		}
		const session = getSession();
		const effects = [libraryActivityChanged(session?.userId)];
		if (session?.isAdmin) effects.push(libraryOperationsChanged());
		void applyEffects(...effects);
	}

	function baseline(): Record<string, number> | undefined {
		const userId = getSession()?.userId;
		if (!userId) return undefined;
		return queryClient.getQueryData<LibraryActivityResponse>(
			LibraryQueryKeyFactory.activity(userId)
		)?.revisions;
	}

	// The first revision set is compared with the activity the page loaded;
	// until that loads, it waits.
	function reconcilePending(): void {
		if (!pendingInitial) return;
		const base = baseline();
		if (!base) return;
		const pending = pendingInitial;
		pendingInitial = null;
		unsubscribeCache?.();
		unsubscribeCache = null;
		apply(base, pending);
	}

	function parse(data: ServerEventData<'activity.changed'>): Record<string, number> | null {
		if (!data?.revisions || typeof data.revisions !== 'object') return null;
		const parsed: Record<string, number> = {};
		for (const [kind, revision] of Object.entries(data.revisions)) {
			if (typeof revision !== 'number' || !Number.isSafeInteger(revision) || revision < 0) {
				return null;
			}
			parsed[kind] = revision;
		}
		return parsed;
	}

	return {
		receive(data) {
			const next = parse(data);
			if (next === null) return;
			if (revisions === null) {
				revisions = next;
				const base = baseline();
				if (base) {
					apply(base, next);
				} else {
					pendingInitial = next;
					unsubscribeCache ??= queryClient.getQueryCache().subscribe(reconcilePending);
				}
				return;
			}
			const previous = revisions;
			revisions = next;
			apply(previous, next);
		},
		reset() {
			if (catalogTimer !== null) clearTimeout(catalogTimer);
			catalogTimer = null;
			catalogPending = false;
			revisions = null;
			pendingInitial = null;
			unsubscribeCache?.();
			unsubscribeCache = null;
		}
	};
}

type LiveListener<N extends ServerEventName> = (data: ServerEventData<N>) => void;

/**
 * The one way server events reach the app. Each event runs its table entry
 * (refreshes, handler, toast), then the live views listening for it.
 */
export function createEventDispatcher(mux: MuxEventStream = muxEventStream) {
	let session: EventSession | null = null;
	let unsubs: MuxUnsubscribe[] = [];
	let seenToasts: string[] = [];
	const live = new Map<ServerEventName, Set<(data: unknown) => void>>();
	const activity = createActivityTracker(() => session);

	function showToast(toast: EventToast | null): void {
		if (!toast || seenToasts.includes(toast.id)) return;
		seenToasts = [...seenToasts, toast.id].slice(-MAX_SEEN_TOASTS);
		saveSeenToasts(seenToasts);
		toastStore.show({ message: toast.message, type: toast.type });
	}

	function dispatch<N extends ServerEventName>(name: N, event: Event): void {
		let data: ServerEventData<N>;
		try {
			data = JSON.parse((event as MessageEvent<string>).data) as ServerEventData<N>;
		} catch {
			return;
		}
		if (data === null || typeof data !== 'object') return;
		const entry = EVENT_TABLE[name] as EventEntry<N>;
		if (session) {
			if (entry.effects) void applyEffects(...entry.effects(data, session));
			entry.handle?.(data, { session, activity });
			if (entry.toast) showToast(entry.toast(data));
		}
		for (const listener of [...(live.get(name) ?? [])]) listener(data);
	}

	function reconnected(): void {
		if (session) void applyEffects(...reconnectEffects(session));
	}

	function stop(): void {
		for (const unsub of unsubs) unsub();
		unsubs = [];
		session = null;
		activity.reset();
	}

	return {
		/** Starts handling events for a signed-in session. */
		start(next: EventSession): void {
			stop();
			session = next;
			seenToasts = [...new Set([...loadSeenToasts(), ...seenToasts])].slice(-MAX_SEEN_TOASTS);
			const names = Object.keys(EVENT_TABLE) as ServerEventName[];
			unsubs = [
				...names.map((name) => mux.on(name, (event) => dispatch(name, event))),
				mux.onConnect(reconnected)
			];
			// Already connected: no first open will come to refresh.
			if (mux.isConnected) reconnected();
		},
		stop,
		/** Listens for one event's data while a live view is open. */
		on<N extends ServerEventName>(name: N, listener: LiveListener<N>): MuxUnsubscribe {
			let set = live.get(name);
			if (!set) live.set(name, (set = new Set()));
			const wrapped = listener as (data: unknown) => void;
			set.add(wrapped);
			return () => {
				set.delete(wrapped);
			};
		}
	};
}

export type EventDispatcher = ReturnType<typeof createEventDispatcher>;

export const serverEvents = createEventDispatcher();
