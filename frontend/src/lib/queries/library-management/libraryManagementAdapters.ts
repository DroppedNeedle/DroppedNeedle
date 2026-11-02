import type { components } from '$lib/api/v3/openapi';
import type {
	LibraryManagementOperationHistoryResponse,
	LibraryManagementPlanItem,
	LibraryManagementPlanItemPageResponse,
	LibraryManagementPreviewDetailResponse,
	LibraryManagementResultPageResponse,
	LibraryManagementTagEditorContext,
	LibraryManagementTagEditorField,
	LibraryManagementTagEditValue
} from './types';

type JobView = components['schemas']['ManagementJobView'];
type TagEditorView = components['schemas']['ManagementTagEditorView'];
type PlanItemView = components['schemas']['ManagementPlanItemView'];
type PlanItemPage = components['schemas']['Page_ManagementPlanItemView'];
type ResultPage = components['schemas']['Page_ManagementResultItemView'];
type HistoryPage = components['schemas']['Page_ManagementHistoryItemView'];

function record(value: unknown): Record<string, unknown> {
	return value && typeof value === 'object' && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: {};
}

function records(value: unknown): Array<Record<string, unknown>> {
	return Array.isArray(value) ? value.map(record) : [];
}

function toPlanItem(view: PlanItemView): LibraryManagementPlanItem {
	return {
		...view,
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
			...item,
			plan: toPlanItem(item.plan),
			result: record(item.result)
		})),
		next_cursor: page.next_cursor ?? null
	};
}

export function toPreviewDetail(view: JobView): LibraryManagementPreviewDetailResponse {
	return { ...view, selection: record(view.selection) };
}

export function toTagEditor(view: TagEditorView): LibraryManagementTagEditorContext {
	return {
		...view,
		fields: view.fields.map((field) => ({
			...field,
			scope: field.scope === 'album' ? 'album' : 'track',
			cardinality: CARDINALITIES.find((value) => value === field.cardinality) ?? 'string',
			current_value: toTagValue(field.current_value),
			override_mode: OVERRIDE_MODES.find((value) => value === field.override_mode) ?? null
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

export function toHistory(page: HistoryPage): LibraryManagementOperationHistoryResponse {
	return {
		items: (page.items ?? []).map((item) => ({ ...item, selection: record(item.selection) })),
		next_cursor: page.next_cursor ?? null
	};
}
