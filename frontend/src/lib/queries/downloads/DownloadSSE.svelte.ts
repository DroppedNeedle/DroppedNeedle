import { API } from '$lib/constants';
import { getApiUrl } from '$lib/api/api-utils';
import type { DownloadProgress, DownloadSourceUpdate } from '$lib/types';

interface DownloadStreamState {
	progress: DownloadProgress | null;
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

	function stop() {
		if (source) {
			source.close();
			source = null;
		}
	}

	function start(taskId: string) {
		stop();
		state = { progress: null, status: null, source: null, done: false };
		source = new EventSource(getApiUrl(API.downloads.stream(taskId)));
		source.addEventListener('status', (e) => {
			const d = parse(e);
			const sourceUpdate = parseSourceUpdate(d, state.source);
			state = {
				...state,
				status: (d.status as string) ?? state.status,
				source: sourceUpdate
			};
		});
		source.addEventListener('progress', (e) => {
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
			const d = parse(e);
			state = { ...state, status: (d.status as string) ?? state.status, done: true };
			stop();
		});
	}

	return {
		get state() {
			return state;
		},
		start,
		stop
	};
}
