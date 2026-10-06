import type { components } from '$lib/api/v3/openapi';
import type {
	LibraryIdentificationPolicy,
	LibraryWorkState,
	ScanKind,
	ScanRun,
	ScanRunCurrentResponse,
	ScanRunDetailResponse
} from './LibraryOperationsTypes';

type ScanRunView = components['schemas']['ScanRunView'];
type CurrentRunsView = components['schemas']['ScanRunCurrentResponse'];
type RunDetailResponse = components['schemas']['RunDetailResponse'];

// The v3 run view carries every v2 run field as a plain string; these
// adapters narrow them to the UI's unions. Diagnostics export has no v3
// route yet, so runs say so and the export button stays hidden.

export function toScanRun(view: ScanRunView): ScanRun {
	return {
		id: view.id,
		kind: view.kind as ScanKind,
		trigger: view.trigger as ScanRun['trigger'],
		state: view.state as LibraryWorkState,
		phase: view.phase as ScanRun['phase'],
		requested_by_user_id: view.requested_by_user_id ?? null,
		aggregate_scope: view.aggregate_scope,
		queued_at: view.queued_at,
		started_at: view.started_at ?? null,
		updated_at: view.updated_at,
		terminal_at: view.terminal_at ?? null,
		resume_phase: (view.resume_phase ?? null) as ScanRun['resume_phase'],
		requested_control: view.requested_control as ScanRun['requested_control'],
		terminal_code: view.terminal_code ?? null,
		coalesced_request_count: view.coalesced_request_count,
		row_revision: view.row_revision,
		event_revision: view.event_revision,
		counters: view.counters,
		phase_timings: view.phase_timings,
		diagnostics_available: false
	};
}

export function toCurrentRuns(runs: CurrentRunsView): ScanRunCurrentResponse {
	return {
		active: runs.active ? toScanRun(runs.active) : null,
		queued: runs.queued ? toScanRun(runs.queued) : null
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
