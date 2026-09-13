import { getApiUrl } from '$lib/api/api-utils';
import { API } from '$lib/constants';
import type { DownloadProgress, DownloadSourceUpdate } from '$lib/types';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import { getDownloadScope } from './downloadScope.svelte';

let structuralRefreshGeneration: number | null = null;

function refreshObservedActivity() {
	const scope = getDownloadScope();
	if (structuralRefreshGeneration === scope.generation) return;
	structuralRefreshGeneration = scope.generation;
	setTimeout(() => {
		if (structuralRefreshGeneration === scope.generation) structuralRefreshGeneration = null;
		if (!scope.userId || scope !== getDownloadScope()) return;
		void invalidateQueriesWithPersister({
			queryKey: DownloadQueryKeyFactory.activity(scope.userId),
			exact: true
		}).catch(() => undefined);
	}, 100);
}

interface DownloadStreamState {
	progress: DownloadProgress | null;
	status: string | null;
	source: DownloadSourceUpdate | null;
	done: boolean;
}

export type OrganizerRetryState = 'running' | 'complete' | 'failed';
export type OrganizerRetryStage = 'preparing' | 'planning' | 'publishing' | 'finalizing';

// Hand-mirror of the backend `organizer_retry` SSE payload on the
// download:{task_id} channel (RetryProgress plan). Full-state snapshots: every
// event carries the whole retry state, so the latest event always wins.
export interface OrganizerRetrySnapshot {
	state: OrganizerRetryState;
	stage: OrganizerRetryStage;
	files_completed: number;
	files_total: number;
	files_imported?: number | null;
	error?: string | null;
	updated_at: string | null;
}

interface OrganizerRetryEntry {
	snapshot: OrganizerRetrySnapshot;
	settled: boolean;
	generation: number;
}

// In-memory only: keyed by task id, latest snapshot wins. Deliberately outside
// the TanStack cache so it is never hydrated from the IndexedDB persister - a
// refresh starts empty and re-attaches from the live SSE snapshot instead of
// replaying stale progress. Consumers RESET the entry alongside query
// invalidation (resetOrganizerRetry) once they have settled from it.
const organizerRetryByTask = $state<Record<string, OrganizerRetryEntry | undefined>>({});

const ORGANIZER_RETRY_STATES: readonly OrganizerRetryState[] = ['running', 'complete', 'failed'];
const ORGANIZER_RETRY_STAGES: readonly OrganizerRetryStage[] = [
	'preparing',
	'planning',
	'publishing',
	'finalizing'
];

function finiteNumber(value: unknown, fallback: number): number {
	const n = Number(value);
	return Number.isFinite(n) ? n : fallback;
}

function parseOrganizerRetry(data: Record<string, unknown>): OrganizerRetrySnapshot | null {
	if (
		!ORGANIZER_RETRY_STATES.includes(data.state as OrganizerRetryState) ||
		!ORGANIZER_RETRY_STAGES.includes(data.stage as OrganizerRetryStage)
	) {
		return null;
	}
	return {
		state: data.state as OrganizerRetryState,
		stage: data.stage as OrganizerRetryStage,
		files_completed: finiteNumber(data.files_completed, 0),
		files_total: finiteNumber(data.files_total, 0),
		files_imported: data.files_imported == null ? null : finiteNumber(data.files_imported, 0),
		error: typeof data.error === 'string' && data.error ? data.error : null,
		updated_at: typeof data.updated_at === 'string' && data.updated_at ? data.updated_at : null
	};
}

function recordOrganizerRetry(taskId: string, data: Record<string, unknown>): void {
	const snapshot = parseOrganizerRetry(data);
	if (!snapshot) return;
	const generation = getDownloadScope().generation;
	const previous = organizerRetryByTask[taskId];
	// Terminal snapshots settle exactly once: a complete|failed entry rejects every
	// later event (including a contradictory terminal) until the consumer resets it.
	if (previous && previous.settled && previous.generation === generation) return;
	organizerRetryByTask[taskId] = {
		snapshot,
		settled: snapshot.state !== 'running',
		generation
	};
}

export function getOrganizerRetry(taskId: string): OrganizerRetrySnapshot | null {
	const entry = organizerRetryByTask[taskId];
	if (!entry || entry.generation !== getDownloadScope().generation) return null;
	return entry.snapshot;
}

export function resetOrganizerRetry(taskId: string): void {
	organizerRetryByTask[taskId] = undefined;
}

function parse(event: Event): Record<string, unknown> {
	try {
		return JSON.parse((event as MessageEvent).data) as Record<string, unknown>;
	} catch {
		return {};
	}
}

function nullableNumber(
	data: Record<string, unknown>,
	key: string,
	previous: number | null
): number | null {
	if (!(key in data)) return previous;
	return data[key] == null ? null : Number(data[key]);
}

function nullableString(
	data: Record<string, unknown>,
	key: string,
	previous: string | null
): string | null {
	if (!(key in data)) return previous;
	return data[key] == null ? null : String(data[key]);
}

function parseSourceUpdate(
	data: Record<string, unknown>,
	previous: DownloadSourceUpdate | null
): DownloadSourceUpdate {
	return {
		candidate_index: nullableNumber(data, 'candidate_index', previous?.candidate_index ?? null),
		source: nullableString(data, 'source', previous?.source ?? null),
		quality_format: nullableString(data, 'quality_format', previous?.quality_format ?? null),
		quality_bit_depth: nullableNumber(
			data,
			'quality_bit_depth',
			previous?.quality_bit_depth ?? null
		),
		quality_sample_rate: nullableNumber(
			data,
			'quality_sample_rate',
			previous?.quality_sample_rate ?? null
		),
		advertised_queue_depth: nullableNumber(
			data,
			'advertised_queue_depth',
			previous?.advertised_queue_depth ?? null
		),
		queue_position_start: nullableNumber(
			data,
			'queue_position_start',
			previous?.queue_position_start ?? null
		),
		queue_position_end: nullableNumber(
			data,
			'queue_position_end',
			previous?.queue_position_end ?? null
		),
		remote_queued:
			'remote_queued' in data ? Boolean(data.remote_queued) : (previous?.remote_queued ?? false),
		preferred_quality_fallback_at: nullableNumber(
			data,
			'preferred_quality_fallback_at',
			previous?.preferred_quality_fallback_at ?? null
		),
		attempt_number: Number(data.attempt_number ?? data.attempt ?? previous?.attempt_number ?? 0),
		attempt_total: Number(data.attempt_total ?? previous?.attempt_total ?? 0),
		has_next_source:
			'has_next_source' in data
				? Boolean(data.has_next_source)
				: (previous?.has_next_source ?? false)
	};
}

// EventSource authenticates via the droppedneedle_session cookie (no custom headers).
// no 'error' handler so keepalive gaps/close don't clobber a terminal state
export function createDownloadStream() {
	let state = $state<DownloadStreamState>({
		progress: null,
		status: null,
		source: null,
		done: false
	});
	let source: EventSource | null = null;
	let streamScope = getDownloadScope();

	function stop() {
		if (source) {
			source.close();
			source = null;
		}
	}

	function start(taskId: string) {
		stop();
		state = { progress: null, status: null, source: null, done: false };
		const scope = getDownloadScope();
		streamScope = scope;
		source = new EventSource(getApiUrl(API.downloads.stream(taskId)));
		source.addEventListener('status', (e) => {
			if (scope !== getDownloadScope()) return;
			const d = parse(e);
			const sourceUpdate = parseSourceUpdate(d, state.source);
			if (
				state.status !== d.status ||
				state.source?.candidate_index !== sourceUpdate.candidate_index ||
				state.source?.attempt_number !== sourceUpdate.attempt_number ||
				state.source?.attempt_total !== sourceUpdate.attempt_total
			) {
				refreshObservedActivity();
			}
			state = {
				...state,
				status: (d.status as string) ?? state.status,
				source: sourceUpdate
			};
		});
		source.addEventListener('progress', (e) => {
			if (scope !== getDownloadScope()) return;
			const d = parse(e);
			const sourceUpdate = parseSourceUpdate(d, state.source);
			state = {
				...state,
				progress: {
					...sourceUpdate,
					bytes_downloaded: Number(d.bytes_downloaded ?? 0),
					bytes_total: Number(d.bytes_total ?? 0),
					files_completed: Number(d.files_completed ?? 0),
					files_total: Number(d.files_total ?? 0),
					progress_percent: Number(d.progress_percent ?? 0)
				},
				source: sourceUpdate
			};
		});
		source.addEventListener('complete', (e) => {
			if (scope !== getDownloadScope()) return;
			refreshObservedActivity();
			const d = parse(e);
			state = { ...state, status: (d.status as string) ?? state.status, done: true };
			stop();
		});
		// Organizer retry progress shares this connection; snapshots land in the
		// task-keyed store (latest wins, terminal settles once). Unlike `complete`
		// this never closes the stream - the card owns that lifecycle.
		source.addEventListener('organizer_retry', (e) => {
			if (scope !== getDownloadScope()) return;
			recordOrganizerRetry(taskId, parse(e));
		});
	}

	return {
		get state() {
			return streamScope === getDownloadScope()
				? state
				: { progress: null, status: null, source: null, done: false };
		},
		start,
		stop
	};
}
