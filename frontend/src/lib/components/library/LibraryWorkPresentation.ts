import type { LibraryWorkItem } from '$lib/queries/library/LibraryOperationsTypes';

const phaseLabels: Record<string, string> = {
	discovering: 'Counting local files',
	indexing: 'Reading file metadata',
	reconciling: 'Finalizing the catalog',
	identifying_albums: 'Matching albums with MusicBrainz',
	planning: 'Inspecting files and release bundles',
	ready: 'Ready for review',
	applying: 'Applying planned changes',
	undoing: 'Restoring the previous file state',
	restoring: 'Restoring original state',
	preparing_snapshots: 'Preparing recovery snapshots',
	writing_staged_files: 'Writing staged files',
	validating_staged_files: 'Validating staged files',
	publishing_files: 'Publishing verified files',
	committing_catalog: 'Committing the catalog',
	cleaning_up: 'Cleaning up',
	checking_identities: 'Checking exact release identities',
	checking_exact_edition: 'Checking the selected MusicBrainz edition',
	applying_identity_decisions: 'Applying identity decisions',
	recovery: 'Recovery needs administrator attention',
	working: 'Working'
};

export function libraryWorkTitle(item: LibraryWorkItem): string {
	if (item.kind === 'scan')
		return item.state === 'failed' ? 'Library scan failed' : 'Scanning library';
	if (item.kind === 'identification')
		return item.state === 'failed' ? 'Album identification failed' : 'Identifying changed albums';
	if (item.kind === 'identity_preparation') return 'Preparing exact MusicBrainz identities';
	if (item.kind === 'reidentification') return 'Checking an exact MusicBrainz edition';
	if (item.kind === 'identity_review') return 'Applying identity review decisions';
	if (item.kind === 'recovery') return 'File recovery needs attention';
	if (item.kind === 'maintenance') return 'Maintaining the library catalog';
	if (item.kind === 'library_management') {
		if (item.state === 'failed') {
			if (item.mode === 'preview') return 'Organization preview failed';
			if (item.mode === 'undo') return 'Organization Undo failed';
			if (item.mode === 'baseline_restore') return 'Original-state restore failed';
			if (item.mode === 'duplicate_resolution') return 'Duplicate resolution failed';
			return 'Applying organization changes failed';
		}
		if (item.mode === 'undo') return 'Undoing organization changes';
		if (item.mode === 'baseline_restore') return 'Restoring original state';
		if (item.mode === 'duplicate_resolution') return 'Resolving duplicate files';
		if (item.effect === 'file_writing') return 'Writing tags and organizing files';
		return 'Preparing a Picard-style preview';
	}
	return 'Library maintenance';
}

export function libraryWorkPhase(item: LibraryWorkItem): string {
	if (item.state === 'queued') return 'Queued';
	if (item.state === 'pausing') {
		return item.kind === 'scan'
			? 'Pausing after the current file'
			: 'Pausing after the current album';
	}
	if (item.state === 'paused') return 'Paused';
	if (item.state === 'stopping') {
		return item.kind === 'scan'
			? 'Stopping after the current file'
			: 'Stopping after the current album';
	}
	if (item.state === 'failed') {
		if (item.kind === 'recovery') return phaseLabels.recovery;
		const failedPhase = phaseLabels[item.phase ?? ''];
		return failedPhase
			? `Failed while ${failedPhase.charAt(0).toLowerCase()}${failedPhase.slice(1)}`
			: 'Needs attention';
	}
	return phaseLabels[item.phase ?? ''] ?? titleCase(item.phase ?? item.state);
}

export function libraryWorkEffect(item: LibraryWorkItem): string {
	if (item.effect === 'file_writing') return 'Writes music files';
	if (item.effect === 'attention') return 'Needs attention';
	return 'Music files stay unchanged';
}

export function libraryWorkProgress(item: LibraryWorkItem): string {
	if (item.remaining_count !== null) {
		return `${item.remaining_count.toLocaleString()} ${plural(item.unit, item.remaining_count)} remaining`;
	}
	if (item.total !== null && item.total > 0) {
		return `${item.processed.toLocaleString()} / ${item.total.toLocaleString()} ${plural(item.unit, item.total)}`;
	}
	if (item.processed > 0) {
		return `${item.processed.toLocaleString()} ${plural(item.unit, item.processed)} processed`;
	}
	return item.state === 'queued' ? 'Waiting to start' : 'Starting…';
}

export function libraryWorkPercentage(item: LibraryWorkItem): number | null {
	if (item.indeterminate || item.total === null || item.total <= 0) return null;
	return Math.min(100, Math.round((item.processed / item.total) * 100));
}

export function libraryWorkHref(item: LibraryWorkItem): string {
	if (item.kind === 'library_management') {
		return `/library/management/operations/${encodeURIComponent(item.id)}`;
	}
	if (
		item.kind === 'identity_preparation' ||
		item.kind === 'reidentification' ||
		item.kind === 'identity_review'
	) {
		return '/library/management?tab=organize';
	}
	if (item.kind === 'recovery') return '/library/management?tab=organize';
	return '/library/management?tab=scanning';
}

export function libraryWorkContext(item: LibraryWorkItem): string | null {
	const values = [item.profile_name, item.scope_label, item.origin ? titleCase(item.origin) : null];
	return values.filter((value): value is string => Boolean(value)).join(' · ') || null;
}

export function libraryWorkFacts(item: LibraryWorkItem): string[] {
	if (item.kind === 'scan') {
		return [
			item.new_count ? `${item.new_count.toLocaleString()} new` : null,
			item.changed_count ? `${item.changed_count.toLocaleString()} changed` : null,
			item.missing_count ? `${item.missing_count.toLocaleString()} missing` : null,
			item.failed_count ? `${item.failed_count.toLocaleString()} errors` : null
		].filter((value): value is string => value !== null);
	}
	if (item.kind === 'library_management') {
		return [
			item.subject_count ? `${item.subject_count.toLocaleString()} files` : null,
			item.warning_count ? `${item.warning_count.toLocaleString()} warnings` : null,
			item.blocked_count ? `${item.blocked_count.toLocaleString()} safely excluded` : null,
			item.failed_count ? `${item.failed_count.toLocaleString()} failed` : null,
			item.skipped_count ? `${item.skipped_count.toLocaleString()} skipped` : null
		].filter((value): value is string => value !== null);
	}
	return [
		item.warning_count ? `${item.warning_count.toLocaleString()} deferred` : null,
		item.failed_count ? `${item.failed_count.toLocaleString()} failed` : null
	].filter((value): value is string => value !== null);
}

export const STALE_INPUT_TERMINAL_CODE = 'STALE_INPUT';

export const STALE_INPUT_HINT =
	'Inputs moved since planning. Retry or refresh to rebuild this preview. No files were changed.';

export interface TerminalOperationLike {
	state?: string | null;
	terminal_code?: string | null;
}

/** Pure-invalidation terminal: the job stopped only because its inputs moved. */
export function isStaleInputTerminal(operation: TerminalOperationLike | null | undefined): boolean {
	if (!operation) return false;
	return operation.state === 'failed' && operation.terminal_code === STALE_INPUT_TERMINAL_CODE;
}

export interface FailedGroupSource {
	selection?: Record<string, unknown> | null;
}

/**
 * Group key collapsing duplicate failed cards for the same album.
 * Only single-kind album selections group; anything else stays individual (null).
 */
export function selectionAlbumGroupKey(selection: unknown): string | null {
	if (typeof selection !== 'object' || selection === null || Array.isArray(selection)) {
		return null;
	}
	const record = selection as Record<string, unknown>;
	if (record.kind !== 'albums' || !Array.isArray(record.ids) || record.ids.length === 0) {
		return null;
	}
	const ids = record.ids.filter((id): id is string => typeof id === 'string' && id.length > 0);
	if (ids.length === 0 || ids.length !== (record.ids as unknown[]).length) return null;
	return `albums:${[...ids].sort().join(',')}`;
}

export interface FailedOperationGroup<T> {
	key: string;
	items: T[];
	staleCount: number;
	allStale: boolean;
}

function groupKeyOf(item: FailedGroupSource, index: number): { key: string; grouped: boolean } {
	const groupKey = selectionAlbumGroupKey(item.selection ?? null);
	if (!groupKey) return { key: `job:${index}`, grouped: false };
	return { key: groupKey, grouped: true };
}

/** Collapse duplicate failed cards for the same album, preserving first-appearance order. */
export function groupFailedOperationsByAlbum<T extends FailedGroupSource>(
	items: T[],
	isStale: (item: T) => boolean
): Array<FailedOperationGroup<T>> {
	const groups = new Map<string, FailedOperationGroup<T>>();
	items.forEach((item, index) => {
		const { key, grouped } = groupKeyOf(item, index);
		// Ungroupable members each keep a unique key, so they never collapse.
		const mapKey = grouped ? key : `${key}:${index}`;
		const existing = groups.get(mapKey);
		if (existing) {
			existing.items.push(item);
			if (isStale(item)) existing.staleCount += 1;
			existing.allStale = existing.staleCount === existing.items.length;
			return;
		}
		const stale = isStale(item);
		groups.set(mapKey, {
			key: mapKey,
			items: [item],
			staleCount: stale ? 1 : 0,
			allStale: stale
		});
	});
	return [...groups.values()];
}

const ACTIVE_SCAN_STATES = new Set([
	'discovering',
	'indexing',
	'reconciling',
	'running',
	'pausing'
]);

/** True while a scan work item is actively holding the library worker. */
export function scanIsActive(items: Array<Pick<LibraryWorkItem, 'kind' | 'state'>>): boolean {
	return items.some((item) => item.kind === 'scan' && ACTIVE_SCAN_STATES.has(item.state));
}

/** A queued organization preview that cannot start while the worker is busy. */
export function isQueuedPreview(item: Pick<LibraryWorkItem, 'kind' | 'state' | 'mode'>): boolean {
	return (
		item.kind === 'library_management' &&
		item.state === 'queued' &&
		(item.mode ?? 'preview') === 'preview'
	);
}

export const WAITING_FOR_SCAN_HINT =
	'Waiting for scan - planning starts after the active scan finishes.';

function plural(unit: LibraryWorkItem['unit'], count: number): string {
	if (count === 1) return unit === 'releases' ? 'release' : unit.slice(0, -1);
	return unit;
}

function titleCase(value: string): string {
	return value.replaceAll('_', ' ').replace(/\b\w/g, (letter) => letter.toUpperCase());
}
