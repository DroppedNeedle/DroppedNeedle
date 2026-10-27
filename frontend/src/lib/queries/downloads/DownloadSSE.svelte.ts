import {
	muxEventStream,
	type MuxEventStream,
	type MuxUnsubscribe
} from '$lib/queries/events/MuxEventStream';
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

// The source fields of a task, kept across `download_progress` events: an
// event that leaves a field out keeps its last value.
export interface DownloadSourceUpdate {
	candidate_index: number | null;
	source: string | null;
	quality_format?: string | null;
	quality_bit_depth?: number | null;
	quality_sample_rate: number | null;
	advertised_queue_depth: number | null;
	queue_position_start: number | null;
	queue_position_end: number | null;
	remote_queued: boolean;
	preferred_quality_fallback_at: number | null;
	attempt_number: number;
	attempt_total: number;
	has_next_source: boolean;
}

interface DownloadStreamProgress extends DownloadSourceUpdate {
	bytes_downloaded: number;
	bytes_total: number;
	files_completed: number;
	files_total: number;
	progress_percent: number;
}

interface DownloadStreamState {
	progress: DownloadStreamProgress | null;
	status: string | null;
	source: DownloadSourceUpdate | null;
	done: boolean;
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

// Live progress for one task, read from the tab's shared event stream: the
// worker sends `download_progress` to the task owner after every poll. Status
// changes reach the cards through the queue list, which `downloads.changed`
// refreshes (see DownloadEvents), so this only carries bytes and source.
export function createDownloadStream(mux: MuxEventStream = muxEventStream) {
	let state = $state<DownloadStreamState>({
		progress: null,
		status: null,
		source: null,
		done: false
	});
	let unsubscribes: MuxUnsubscribe[] = [];
	let streamScope = getDownloadScope();

	function stop() {
		for (const unsubscribe of unsubscribes) unsubscribe();
		unsubscribes = [];
	}

	function start(taskId: string) {
		stop();
		state = { progress: null, status: null, source: null, done: false };
		const scope = getDownloadScope();
		streamScope = scope;
		const unsubscribe = mux.on('download_progress', (e) => {
			if (scope !== getDownloadScope()) return;
			const d = parse(e);
			if (d.task_id !== taskId) return;
			const sourceUpdate = parseSourceUpdate(d, state.source);
			if (
				state.status !== d.status ||
				state.source?.candidate_index !== sourceUpdate.candidate_index ||
				state.source?.attempt_number !== sourceUpdate.attempt_number
			) {
				refreshObservedActivity();
			}
			state = {
				...state,
				status: typeof d.status === 'string' ? d.status : state.status,
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
		unsubscribes.push(unsubscribe);
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
