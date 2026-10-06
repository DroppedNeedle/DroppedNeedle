import type { components } from '$lib/api/v3/openapi';
import type {
	LibraryIdentificationPolicy,
	LibraryWorkState,
	ScanKind,
	ScanRun,
	ScanRunCurrentResponse,
	ScanRunDetailResponse,
	ScanRunRequestedResponse,
	TargetLibrarySettingsResponse
} from './LibraryOperationsTypes';

type ScanRunView = components['schemas']['ScanRunView'];
type ScanRunsResponse = components['schemas']['ScanRunsResponse'];
type RunDetailResponse = components['schemas']['RunDetailResponse'];
type ScanResponse = components['schemas']['ScanResponse'];

// v3 scan runs carry the state, phase, counters and timestamps. Pause,
// resume and stop controls, coalescing and per-phase timings have no v3
// source yet, so those read as "never requested" here.

export function toScanRun(view: ScanRunView): ScanRun {
	return {
		id: view.id,
		kind: view.kind as ScanKind,
		trigger: view.trigger as ScanRun['trigger'],
		state: view.state as LibraryWorkState,
		phase: view.phase as ScanRun['phase'],
		requested_by_user_id: null,
		aggregate_scope: view.aggregate_scope,
		queued_at: view.queued_at,
		started_at: view.started_at ?? null,
		updated_at: view.updated_at,
		terminal_at: view.terminal_at ?? null,
		resume_phase: null,
		requested_control: 'none',
		terminal_code: view.terminal_code ?? null,
		coalesced_request_count: 0,
		row_revision: 0,
		event_revision: 0,
		counters: view.counters,
		phase_timings: {},
		controls_available: false
	};
}

export function toCurrentRuns(runs: ScanRunsResponse): ScanRunCurrentResponse {
	const active = runs.current.find((run) => run.state !== 'queued');
	const queued = runs.current.find((run) => run.state === 'queued');
	return {
		active: active ? toScanRun(active) : null,
		queued: queued ? toScanRun(queued) : null
	};
}

export function toRunDetail(detail: RunDetailResponse): ScanRunDetailResponse {
	return {
		snapshot: {
			run: toScanRun(detail.run),
			scopes: detail.scopes.map((scope) => ({
				root_id: scope.root_id,
				scope_id: null,
				relative_path: scope.relative_path,
				effective_policy: scope.effective_policy as LibraryIdentificationPolicy,
				policy_revision: '',
				estimated_count: null
			})),
			counters: detail.run.counters
		}
	};
}

export function toRunRequested(response: ScanResponse): ScanRunRequestedResponse {
	return {
		run_id: response.run_id,
		disposition: response.disposition as ScanRunRequestedResponse['disposition'],
		state: response.state as LibraryWorkState,
		row_revision: 0,
		queued_reason: null,
		conflicting_kind: null,
		estimated_file_count: null
	};
}

// v3 scans whole roots. A scope id names a root or a path rule inside one,
// so each id resolves to its root through the saved settings; an id the
// settings do not know widens the request to every root (null).
export function scanRootsFor(
	scopeIds: string[],
	settings: TargetLibrarySettingsResponse | undefined
): Array<string | null> {
	if (scopeIds.length === 0 || !settings) return [null];
	const rootOf = new Map<string, string>();
	for (const root of settings.library_roots) {
		rootOf.set(root.id, root.id);
		for (const rule of root.rules) rootOf.set(rule.id, root.id);
	}
	const roots = new Set<string>();
	for (const id of scopeIds) {
		const root = rootOf.get(id);
		if (!root) return [null];
		roots.add(root);
	}
	return [...roots].sort();
}
