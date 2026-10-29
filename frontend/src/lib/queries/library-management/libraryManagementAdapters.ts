import type { components } from '$lib/api/v3/openapi';
import { toOperationResponse } from '$lib/queries/library/libraryOperationAdapters';
import type {
	LibraryManagementOperationHistoryResponse,
	LibraryManagementPlanItem,
	LibraryManagementPlanItemPageResponse,
	LibraryManagementPreviewDetailResponse,
	LibraryManagementReason,
	LibraryManagementRecoveryDiagnosticsResponse,
	LibraryManagementResultPageResponse,
	LibraryManagementTagEditorContext,
	LibraryManagementTagEditorField,
	LibraryManagementTagEditValue,
	ManagementEligibility
} from './types';

type JobView = components['schemas']['ManagementJobView'];
type TagEditorView = components['schemas']['ManagementTagEditorView'];
type RecoveryView = components['schemas']['ManagementRecoveryView'];
type PlanItemView = components['schemas']['ManagementPlanItemView'];
type PlanItemPage = components['schemas']['Page_ManagementPlanItemView'];
type ResultPage = components['schemas']['Page_ManagementResultItemView'];
type HistoryPage = components['schemas']['Page_ManagementHistoryItemView'];
type ReasonView = components['schemas']['ManagementReasonView'];

const ELIGIBILITY: ManagementEligibility[] = ['eligible', 'warning', 'blocked', 'stale'];

function record(value: unknown): Record<string, unknown> {
	return value && typeof value === 'object' && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: {};
}

function records(value: unknown): Array<Record<string, unknown>> {
	return Array.isArray(value) ? value.map(record) : [];
}

function toReason(view: ReasonView | null | undefined): LibraryManagementReason | null {
	return view ? { code: view.code, message: view.message, action: view.action } : null;
}

export function toPlanItem(view: PlanItemView): LibraryManagementPlanItem {
	return {
		ordinal: view.ordinal,
		bundle_ordinal: view.bundle_ordinal,
		local_album_id: view.local_album_id ?? null,
		local_track_id: view.local_track_id ?? null,
		source_root_id: view.source_root_id ?? null,
		source_relative_path: view.source_relative_path ?? null,
		destination_root_id: view.destination_root_id ?? null,
		destination_relative_path: view.destination_relative_path ?? null,
		eligibility: ELIGIBILITY.find((value) => value === view.eligibility) ?? 'blocked',
		reason_code: view.reason_code ?? null,
		reason: toReason(view.reason),
		estimated_temporary_bytes: view.estimated_temporary_bytes,
		desired_document: record(view.desired_document),
		artwork_choices: records(view.artwork_choices),
		diff: record(view.diff),
		capability: record(view.capability),
		collisions: records(view.collisions)
	};
}

export function toPlanItemPage(page: PlanItemPage): LibraryManagementPlanItemPageResponse {
	return {
		items: (page.items ?? []).map(toPlanItem),
		next_cursor: page.next_cursor ?? null
	};
}

export function toResultPage(page: ResultPage): LibraryManagementResultPageResponse {
	return {
		items: (page.items ?? []).map((item) => ({
			plan: toPlanItem(item.plan),
			work_state: item.work_state,
			failure_code: item.failure_code ?? null,
			failure_reason: toReason(item.failure_reason),
			result: record(item.result),
			journal_states: item.journal_states ?? []
		})),
		next_cursor: page.next_cursor ?? null
	};
}

export function toPreviewDetail(view: JobView): LibraryManagementPreviewDetailResponse {
	return {
		...view,
		proposed_settings_revision: view.proposed_settings_revision ?? null,
		target_root_id: view.target_root_id ?? null,
		linked_job_id: view.linked_job_id ?? null,
		selection: record(view.selection),
		summary: {
			...view.summary,
			selected_item_count: view.summary?.selected_item_count ?? null
		},
		expires_at: view.expires_at ?? null,
		terminal_code: view.terminal_code ?? null,
		terminal_reason: view.terminal_reason ?? null,
		worker_heartbeat_at: view.worker_heartbeat_at ?? null,
		worker_lease_expires_at: view.worker_lease_expires_at ?? null,
		undo_expires_at: view.undo_expires_at ?? null,
		external_refreshes: view.external_refreshes
	};
}

export function toTagEditor(view: TagEditorView): LibraryManagementTagEditorContext {
	return {
		...view,
		identity_reason: view.identity_reason ?? null,
		fields: view.fields.map((field) => ({
			field_name: field.field_name,
			scope: field.scope === 'album' ? 'album' : 'track',
			cardinality: CARDINALITIES.find((value) => value === field.cardinality) ?? 'string',
			current_value: toTagValue(field.current_value),
			override_id: field.override_id ?? null,
			override_mode: OVERRIDE_MODES.find((value) => value === field.override_mode) ?? null,
			override_row_revision: field.override_row_revision ?? null
		}))
	};
}

const CARDINALITIES: LibraryManagementTagEditorField['cardinality'][] = [
	'string',
	'integer',
	'boolean',
	'ordered_strings'
];
const OVERRIDE_MODES: NonNullable<LibraryManagementTagEditorField['override_mode']>[] = [
	'replace',
	'preserve',
	'clear'
];

function toTagValue(value: unknown): LibraryManagementTagEditValue {
	if (
		typeof value === 'string' ||
		typeof value === 'number' ||
		typeof value === 'boolean' ||
		value === null
	) {
		return value;
	}
	return Array.isArray(value) ? value.map(String) : null;
}

export function toRecovery(view: RecoveryView): LibraryManagementRecoveryDiagnosticsResponse {
	return {
		...view,
		oldest_updated_at: view.oldest_updated_at ?? null,
		state_counts: view.state_counts ?? {},
		needs_attention_bundles: view.needs_attention_bundles ?? []
	};
}

export function toHistory(page: HistoryPage): LibraryManagementOperationHistoryResponse {
	return {
		items: (page.items ?? []).map((item) => ({
			...item,
			operation: toOperationResponse(item.operation),
			target_root_id: item.target_root_id ?? null,
			selection: record(item.selection),
			expires_at: item.expires_at ?? null
		})),
		next_cursor: page.next_cursor ?? null
	};
}
