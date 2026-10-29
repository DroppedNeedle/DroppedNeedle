import {
	anyLibraryChanged,
	applyEffects,
	downloadTaskListChanged,
	heldFilesChanged
} from '$lib/queries/effects';
import { getDownloadScope } from './downloadScope.svelte';
import type { DownloadActivitySummary } from '$lib/types';

export interface DownloadActivityObservation {
	userId: string | null;
	revision: number | null;
	landed: Set<string>;
	scopeGeneration?: number;
}

export interface DownloadActivityActions {
	refreshDetails: boolean;
	landedMbids: string[];
	invalidateLibrary: boolean;
}

/**
 * Refreshes what a move in the caller's active downloads makes stale: the
 * task list, the held files and, once an album landed, the library.
 */
export function refreshAfterDownloadActivity(
	userId: string,
	actions: DownloadActivityActions
): Promise<void> {
	return applyEffects(
		downloadTaskListChanged(userId),
		heldFilesChanged(userId),
		...(actions.invalidateLibrary ? [anyLibraryChanged()] : [])
	);
}

export function createDownloadActivityObservation(): DownloadActivityObservation {
	return { userId: null, revision: null, landed: new Set() };
}

export function reconcileDownloadActivity(
	observed: DownloadActivityObservation,
	userId: string | null,
	summary: DownloadActivitySummary | undefined
): DownloadActivityActions {
	const generation = getDownloadScope().generation;
	if (observed.scopeGeneration !== generation) {
		observed.scopeGeneration = generation;
		observed.revision = null;
		observed.landed = new Set();
	}
	if (!userId || !summary) {
		observed.userId = userId;
		observed.revision = null;
		observed.landed = new Set();
		return { refreshDetails: false, landedMbids: [], invalidateLibrary: false };
	}

	if (observed.userId !== userId) {
		observed.userId = userId;
		observed.revision = null;
		observed.landed = new Set();
	}

	const landed = new Set(summary.landed_release_group_mbids.map((mbid) => mbid.toLowerCase()));
	const newlyLanded = [...landed].filter((mbid) => !observed.landed.has(mbid));
	const initial = observed.revision === null;
	const refreshDetails = !initial && observed.revision !== summary.revision;

	observed.revision = summary.revision;
	observed.landed = landed;

	return {
		refreshDetails,
		landedMbids: initial ? [] : newlyLanded,
		invalidateLibrary: refreshDetails && newlyLanded.length > 0
	};
}
