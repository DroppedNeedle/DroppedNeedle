import { createMutation, createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import { HomeQueryKeyFactory } from '$lib/queries/HomeQueryKeyFactory';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { DropImportQueryKeyFactory } from '$lib/queries/import/DropImportQueryKeyFactory';
import { LibraryQueryKeyFactory } from '$lib/queries/library/LibraryQueryKeyFactory';
import { LOCAL_KEYS } from '$lib/queries/local/LocalV3Keys';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import type { HeldActionResult, HeldRecheckAllResult } from '$lib/types';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import { HELD_ENDPOINTS, type HeldListFilters } from './endpoints';

// Held files waiting for a decision, one entry per download or upload. Three
// callers: the Downloads queue (every entry), the import tab (an upload's
// entry inside its job) and the album page (one album's). Keyed under tasks()
// so a download mutation's invalidateTasks() refreshes it too; the activity
// revision invalidates this prefix after held rows change.
export const getHeldEntriesQuery = (
	getFilters: Getter<HeldListFilters> = () => ({}),
	getEnabled: Getter<boolean> = () => true
) =>
	createQuery(() => ({
		enabled: getEnabled(),
		staleTime: 30_000,
		queryKey: DownloadQueryKeyFactory.held(authStore.user?.id, getFilters()),
		queryFn: ({ signal }) => api.global.v3.GET(HELD_ENDPOINTS.list(getFilters()), { signal })
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
async function invalidateAfterAction(imported: boolean, albums: (string | null | undefined)[]) {
	const userId = authStore.user?.id;
	const work: Promise<unknown>[] = [
		invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.tasksPrefix(userId) }),
		invalidateQueriesWithPersister({ queryKey: DropImportQueryKeyFactory.prefix })
	];
	for (const mbid of albums) {
		if (mbid) {
			work.push(invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.album(mbid) }));
		}
	}
	if (imported) {
		work.push(
			invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.all }),
			invalidateQueriesWithPersister({ queryKey: LOCAL_KEYS.root }),
			invalidateQueriesWithPersister({ queryKey: HomeQueryKeyFactory.prefix })
		);
	}
	await Promise.all(work);
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
	return createMutation(() => ({
		mutationFn: post,
		onSuccess: async (result: HeldActionResult, input: I) => {
			toastStore.show({
				message: actionSummary(result),
				type: result.imported > 0 ? 'success' : 'info'
			});
			const match = (input as Partial<HeldMatchInput>).matchMbid;
			await invalidateAfterAction(result.imported > 0, [input.releaseGroupMbid, match]);
		},
		onError: (err: unknown, input: I) => {
			toastStore.show({ message: errorMessage(err, failure), type: 'error' });
			void invalidateAfterAction(false, [input.releaseGroupMbid]);
		}
	}));
}

const fileBody = (input: HeldFilesInput) => ({ file_ids: input.fileIds ?? null });

/** Import, Retry, "Use this version" and "Use this file" all place files this way. */
export const importHeldMutation = () =>
	heldMutation(
		(input: HeldFilesInput) =>
			api.global.v3.POST(HELD_ENDPOINTS.import(input.entryId), fileBody(input)),
		'Could not import the held files'
	);

/** Curators only: import the files as the album picked. */
export const matchHeldMutation = () =>
	heldMutation(
		(input: HeldMatchInput) =>
			api.global.v3.POST(HELD_ENDPOINTS.match(input.entryId), {
				release_group_mbid: input.matchMbid,
				file_ids: input.fileIds ?? null
			}),
		'Match failed'
	);

export const recheckHeldMutation = () =>
	heldMutation(
		(input: HeldFilesInput) =>
			api.global.v3.POST(HELD_ENDPOINTS.recheck(input.entryId), fileBody(input)),
		'Could not re-check the held files'
	);

export const discardHeldMutation = () =>
	heldMutation(
		(input: HeldFilesInput) =>
			api.global.v3.POST(HELD_ENDPOINTS.discard(input.entryId), fileBody(input)),
		'Could not discard the held files'
	);

// Re-check AcoustID holds across entries. The server takes at most 25 checks a
// call, so entry ids pass through untouched.
export const recheckAllHeldMutation = () =>
	createMutation(() => ({
		mutationFn: (entryIds: number[] | null) =>
			api.global.v3.POST(HELD_ENDPOINTS.recheckAll(), { entry_ids: entryIds }),
		onSuccess: async (data: HeldRecheckAllResult) => {
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
			await invalidateAfterAction(imported > 0, []);
		},
		onError: (err: unknown) =>
			toastStore.show({ message: errorMessage(err, 'Could not re-check'), type: 'error' })
	}));
