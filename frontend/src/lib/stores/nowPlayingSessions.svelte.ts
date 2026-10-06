import { SvelteMap, SvelteSet } from 'svelte/reactivity';
import { fetchNowPlayingSnapshot, type NowPlayingEntry } from '$lib/player/playbackGateway';
import {
	muxEventStream,
	type MuxEventStream,
	type MuxUnsubscribe
} from '$lib/queries/events/MuxEventStream';

// Client view over one presence row: the v3 entry plus client-only fields.
// audio_codec/bitrate are v2-only and stay absent on v3 rows (the quality
// badge hides itself when they are missing).
export interface NowPlayingSession {
	id: string;
	user_name: string;
	track_name: string;
	artist_name: string;
	album_name: string;
	cover_url: string;
	device_name: string;
	is_paused: boolean;
	source?: string;
	progress_ms?: number | null;
	duration_ms?: number | null;
	redacted?: boolean;
	audio_codec?: string;
	bitrate?: number;
	_isLocal?: boolean;
}

export function nowPlayingEntryToSession(entry: NowPlayingEntry): NowPlayingSession {
	return {
		id: entry.id,
		user_name: entry.user_name,
		track_name: entry.track_name,
		artist_name: entry.artist_name,
		album_name: entry.album_name ?? '',
		cover_url: entry.cover_url,
		device_name: entry.device_name,
		is_paused: entry.is_paused,
		source: entry.source,
		progress_ms: entry.progress_ms,
		duration_ms: entry.duration_ms,
		redacted: entry.redacted
	};
}

// Server-driven presence: hydrate once over HTTP, then receive privacy-projected
// snapshots live over SSE (the `now-playing` channel). The 1s tick only smooths
// the progress bar between snapshots - it never invents sessions.
const TICK_MS = 1_000;
const STALE_PROGRESS_THRESHOLD_MS = 12_000; // no fresh progress -> show as paused
const FROZEN_BASIS_MS = 15_000;
const MAX_INTERPOLATION_ADVANCE_MS = 12_000;

type InterpolationBasis = { serverProgress: number; updatedAt: number };

export function createNowPlayingStore(mux: MuxEventStream = muxEventStream) {
	let sessions = $state<NowPlayingSession[]>([]);
	let unsubSnapshot: MuxUnsubscribe | null = null;
	let tickTimer: ReturnType<typeof setInterval> | undefined;
	let running = false;

	const interpBasis = new SvelteMap<string, InterpolationBasis>();

	const activeSessions = $derived(sessions.filter((s) => !s.is_paused));
	const primarySession = $derived(activeSessions[0] ?? sessions[0] ?? null);

	function applySnapshot(incoming: NowPlayingSession[]): void {
		const now = Date.now();
		const newIds = new SvelteSet<string>();
		for (const s of incoming) {
			newIds.add(s.id);
			if (s.progress_ms == null) continue;
			const prev = interpBasis.get(s.id);
			// refresh the basis on every live (non-paused) snapshot, not only when the
			// position advanced - so a momentarily-unchanged position on a playing track
			// isn't read as stale and shown "Paused" to other viewers
			if (!prev || prev.serverProgress !== s.progress_ms || !s.is_paused) {
				interpBasis.set(s.id, { serverProgress: s.progress_ms, updatedAt: now });
			}
		}
		for (const key of interpBasis.keys()) {
			if (!newIds.has(key)) interpBasis.delete(key);
		}
		// if the live feed went quiet for a session (no snapshot in a while), show it as
		// paused rather than a frozen scrubber. Produce new objects - never mutate the
		// just-parsed SSE payload, which is also handed straight to `sessions`.
		sessions = incoming.map((s) => {
			if (s.is_paused) return s;
			const basis = interpBasis.get(s.id);
			if (basis && now - basis.updatedAt > STALE_PROGRESS_THRESHOLD_MS) {
				return { ...s, is_paused: true };
			}
			return s;
		});
	}

	function tick(): void {
		if (typeof document !== 'undefined' && document.hidden) return;
		const now = Date.now();
		sessions = sessions.map((s) => {
			if (s.is_paused || !s.duration_ms || s.progress_ms == null) return s;
			const basis = interpBasis.get(s.id);
			if (!basis) {
				const next = Math.min(s.progress_ms + TICK_MS, s.duration_ms);
				return next === s.progress_ms ? s : { ...s, progress_ms: next };
			}
			const basisAge = now - basis.updatedAt;
			if (basisAge > FROZEN_BASIS_MS) return s;
			const elapsed = Math.min(basisAge, MAX_INTERPOLATION_ADVANCE_MS);
			const interpolated = Math.min(basis.serverProgress + elapsed, s.duration_ms);
			return interpolated === s.progress_ms ? s : { ...s, progress_ms: interpolated };
		});
	}

	function onSnapshot(event: MessageEvent): void {
		try {
			// same entry shape as GET /now-playing: optional fields may be absent
			const data = JSON.parse(event.data) as { sessions?: NowPlayingEntry[] };
			applySnapshot((data.sessions ?? []).map(nowPlayingEntryToSession));
		} catch {
			// ignore a malformed frame; the next snapshot supersedes it
		}
	}

	async function hydrate(): Promise<void> {
		try {
			const data = await fetchNowPlayingSnapshot();
			applySnapshot((data.sessions ?? []).map(nowPlayingEntryToSession));
		} catch {
			// the SSE snapshot replayed on connect will populate us shortly
		}
	}

	function start(): void {
		if (running) return;
		running = true;
		void hydrate();
		unsubSnapshot = mux.on('snapshot', onSnapshot as EventListener);
		tickTimer = setInterval(tick, TICK_MS);
	}

	function stop(): void {
		running = false;
		unsubSnapshot?.();
		unsubSnapshot = null;
		if (tickTimer) {
			clearInterval(tickTimer);
			tickTimer = undefined;
		}
		sessions = [];
		interpBasis.clear();
	}

	function isSourcePlaying(src: string): boolean {
		return sessions.some((s) => s.source === src && !s.is_paused);
	}

	function sourceHasSessions(src: string): boolean {
		return sessions.some((s) => s.source === src);
	}

	function sessionsForSource(src: string): NowPlayingSession[] {
		return sessions.filter((s) => s.source === src);
	}

	return {
		get sessions() {
			return sessions;
		},
		get activeSessions() {
			return activeSessions;
		},
		get primarySession() {
			return primarySession;
		},
		start,
		stop,
		refresh: hydrate,
		isSourcePlaying,
		sourceHasSessions,
		sessionsForSource
	};
}

export const nowPlayingStore = createNowPlayingStore();
