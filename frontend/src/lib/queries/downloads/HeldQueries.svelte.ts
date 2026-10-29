import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import type { HeldActionResult, HeldRecheckAllResult } from '$lib/types';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import { HELD_ENDPOINTS, type HeldListFilters } from './endpoints';
import {
	albumStatusChanged,
	applyEffects,
	createAppMutation,
	downloadsChanged,
	dropImportsChanged,
	type Effect,
	libraryChanged
} from '$lib/queries/effects';

// Held files waiting for a decision, one entry per download or upload. Three
// callers: the Downloads queue (every entry), the import tab (an upload's
// entry inside its job) and the album page (one album's). Keyed under tasks()
// so a download mutation's refresh of the tasks reaches it too; the activity
// revision invalidates this prefix after held rows change.
export const getHeldEntriesQuery = (
	getFilters: Getter<HeldListFilters> = () => ({}),
	getEnabled: Getter<boolean> = () => true
) =>
	createQuery(() => ({
		enabled: getEnabled(),
		queryKey: DownloadQueryKeyFactory.held(authStore.user?.id, getFilters()),
		queryFn: ({ signal }) => api.v3.GET(HELD_ENDPOINTS.list(getFilters()), { signal })
	}));

function errorMessage(err: unknown, fallback: string): string {
	return err instanceof Error && err.message ? err.message : fallback;
}

function plural(n: number, word: string): string {
	return `${n} ${word}${n === 1 ? '' : 's'}`;
}

// The answer in one line: what moved, and the server's reason when a file stays.
function actionSummary(result: HeldActionResult): string {
	const parts = [
		result.imported > 0 ? `${plural(result.imported, 'file')} imported` : null,
		result.already_owned > 0 ? `${result.already_owned} already in your library` : null,
		result.discarded > 0 ? `${plural(result.discarded, 'file')} discarded` : null,
		result.still_held > 0 ? `${result.still_held} still held` : null
	].filter((part): part is string => part !== null);
	const summary = parts.join(', ') || 'Nothing changed';
	return result.message ? `${summary}. ${result.message}` : summary;
}

// A held action can move files into the library, end a download's hold and
// settle a drop item: refresh the queue (which holds the held lists), the
// import jobs, and, when files went in, the library surfaces that show them.
function afterAction(imported: boolean, albums: (string | null | undefined)[]): Effect[] {
	const userId = authStore.user?.id;
	const effects = [downloadsChanged(userId), dropImportsChanged(userId)];
	for (const mbid of albums) {
		if (mbid) effects.push(albumStatusChanged(userId, mbid));
	}
	if (imported) effects.push(libraryChanged());
	return effects;
}

export interface HeldFilesInput {
	entryId: number;
	/** The files the action takes; every held file of the entry when left out. */
	fileIds?: number[];
	releaseGroupMbid?: string | null;
}

export interface HeldMatchInput extends HeldFilesInput {
	/** The album the curator picked. */
	matchMbid: string;
}

function heldMutation<I extends HeldFilesInput>(
	post: (input: I) => Promise<HeldActionResult>,
	failure: string
) {
	return createAppMutation(() => ({
		mutationFn: post,
		onSuccess: (result: HeldActionResult) => {
			toastStore.show({
				message: actionSummary(result),
				type: result.imported > 0 ? 'success' : 'info'
			});
		},
		effects: (result: HeldActionResult, input: I) => {
			const match = (input as Partial<HeldMatchInput>).matchMbid;
			return afterAction(result.imported > 0, [input.releaseGroupMbid, match]);
		},
		onError: (err: unknown, input: I) => {
			toastStore.show({ message: errorMessage(err, failure), type: 'error' });
			void applyEffects(...afterAction(false, [input.releaseGroupMbid]));
		}
	}));
}

const fileBody = (input: HeldFilesInput) => ({ file_ids: input.fileIds ?? null });

/** Import, Retry, "Use this version" and "Use this file" all place files this way. */
export const importHeldMutation = () =>
	heldMutation(
		(input: HeldFilesInput) => api.v3.POST(HELD_ENDPOINTS.import(input.entryId), fileBody(input)),
		'Could not import the held files'
	);

/** Curators only: import the files as the album picked. */
export const matchHeldMutation = () =>
	heldMutation(
		(input: HeldMatchInput) =>
			api.v3.POST(HELD_ENDPOINTS.match(input.entryId), {
				release_group_mbid: input.matchMbid,
				file_ids: input.fileIds ?? null
			}),
		'Match failed'
	);

export const recheckHeldMutation = () =>
	heldMutation(
		(input: HeldFilesInput) => api.v3.POST(HELD_ENDPOINTS.recheck(input.entryId), fileBody(input)),
		'Could not re-check the held files'
	);

export const discardHeldMutation = () =>
	heldMutation(
		(input: HeldFilesInput) => api.v3.POST(HELD_ENDPOINTS.discard(input.entryId), fileBody(input)),
		'Could not discard the held files'
	);

// Re-check AcoustID holds across entries. The server takes at most 25 checks a
// call, so entry ids pass through untouched.
export const recheckAllHeldMutation = () =>
	createAppMutation(() => ({
		mutationFn: (entryIds: number[] | null) =>
			api.v3.POST(HELD_ENDPOINTS.recheckAll(), { entry_ids: entryIds }),
		onSuccess: (data: HeldRecheckAllResult) => {
			const imported = data.results.filter((r) => r.status === 'imported').length;
			const stillHeld = data.results.filter((r) => r.status === 'still_held').length;
			const failed = data.results.length - imported - stillHeld;
			if (data.results.length === 0) {
				toastStore.show({ message: 'Nothing to re-check', type: 'info' });
			} else {
				const parts = [
					imported > 0 ? `${imported} imported` : null,
					stillHeld > 0 ? `${stillHeld} still held` : null,
					failed > 0 ? `${failed} failed` : null
				].filter((p): p is string => p !== null);
				toastStore.show({
					message: `Re-checked ${data.results.length}: ${parts.join(', ')}`,
					type: imported > 0 ? 'success' : 'info'
				});
			}
		},
		effects: (data: HeldRecheckAllResult) =>
			afterAction(
				data.results.some((r) => r.status === 'imported'),
				[]
			),
		onError: (err: unknown) =>
			toastStore.show({ message: errorMessage(err, 'Could not re-check'), type: 'error' })
	}));
