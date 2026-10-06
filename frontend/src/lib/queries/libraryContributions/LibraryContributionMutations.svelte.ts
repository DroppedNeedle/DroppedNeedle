import { createMutation } from '@tanstack/svelte-query';
import { goto } from '$app/navigation';
import { withBasePath } from '$lib/utils/basePath';
import { api } from '$lib/api/client';
import { LibraryContributionApi } from './LibraryContributionApi';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import type {
	DiscogsReleaseCandidate,
	LibraryContribution,
	MusicBrainzSeed,
	ReleaseDraft
} from '$lib/types';
import { invalidateLibraryCatalog } from '$lib/queries/library/LibraryCatalogInvalidation';
import {
	invalidateQueriesWithPersister,
	setQueryDataWithPersister
} from '$lib/queries/QueryClient';
import { LibraryContributionQueryKeyFactory } from './LibraryContributionQueryKeyFactory';

// Draft saves touch exactly one contribution row; its detail key is already
// fresh via setQueryDataWithPersister above, and no catalog rows change until a
// link lands (attach mutation / page transition guard sweep the catalog then).
const saveContribution = async (contribution: LibraryContribution): Promise<void> => {
	await setQueryDataWithPersister(
		LibraryContributionQueryKeyFactory.detail(authStore.user?.id, contribution.id),
		contribution
	);
	await invalidateQueriesWithPersister({
		queryKey: LibraryContributionQueryKeyFactory.root(authStore.user?.id)
	});
};

// The server says what went wrong and what to do about it
// (`details.action`); show both after the short summary.
const failureMessage = (summary: string, error: unknown): string => {
	if (!(error instanceof Error) || !error.message) return summary;
	const details = (error as { details?: unknown }).details;
	const action =
		details && typeof details === 'object' && 'action' in details
			? String((details as { action: unknown }).action)
			: '';
	return [`${summary}: ${error.message}`, action].filter(Boolean).join(' ');
};

const refreshAfterMutationError = async (
	contributionId: string,
	message: string,
	error: unknown
): Promise<void> => {
	await invalidateQueriesWithPersister({
		queryKey: LibraryContributionQueryKeyFactory.detail(authStore.user?.id, contributionId)
	});
	toastStore.show({ message: failureMessage(message, error), type: 'error' });
};

export const createLibraryContributionMutation = () =>
	createMutation(() => ({
		mutationFn: (albumId: string) =>
			api.global.post<LibraryContribution>(LibraryContributionApi.create(albumId), {}),
		onSuccess: async (contribution) => {
			await saveContribution(contribution);
			toastStore.show({ message: 'Contribution draft ready', type: 'success' });
			await goto(withBasePath(`/library/contributions/${contribution.id}`));
		},
		onError: (error) =>
			toastStore.show({
				message: failureMessage("Couldn't start the contribution", error),
				type: 'error'
			})
	}));

export const updateLibraryContributionMutation = () =>
	createMutation(() => ({
		mutationFn: (input: {
			contributionId: string;
			expectedRowRevision: number;
			draft: ReleaseDraft;
		}) =>
			api.global.put<LibraryContribution>(LibraryContributionApi.draft(input.contributionId), {
				expected_row_revision: input.expectedRowRevision,
				draft: input.draft
			}),
		onSuccess: async (contribution) => {
			await saveContribution(contribution);
			toastStore.show({ message: 'Draft saved', type: 'success' });
		},
		onError: async (error, input) =>
			refreshAfterMutationError(input.contributionId, "Couldn't save the draft", error)
	}));

const revisionMutation = (
	action: 'rebuild' | 'cancel',
	successMessage: string,
	errorMessage: string
) =>
	createMutation(() => ({
		mutationFn: (input: { contributionId: string; expectedRowRevision: number }) => {
			const url =
				action === 'rebuild'
					? LibraryContributionApi.rebuild(input.contributionId)
					: LibraryContributionApi.cancel(input.contributionId);
			return api.global.post<LibraryContribution>(url, {
				expected_row_revision: input.expectedRowRevision
			});
		},
		onSuccess: async (contribution) => {
			await saveContribution(contribution);
			await invalidateQueriesWithPersister({
				queryKey: LibraryContributionQueryKeyFactory.root(authStore.user?.id)
			});
			toastStore.show({ message: successMessage, type: 'success' });
			if (action === 'rebuild') {
				await goto(withBasePath(`/library/contributions/${contribution.id}`), {
					replaceState: true
				});
			}
		},
		onError: async (error, input) =>
			refreshAfterMutationError(input.contributionId, errorMessage, error)
	}));

export const rebuildLibraryContributionMutation = () =>
	revisionMutation('rebuild', 'Draft rebuilt from the current album', "Couldn't rebuild the draft");

export const cancelLibraryContributionMutation = () =>
	revisionMutation('cancel', 'Contribution cancelled', "Couldn't cancel the contribution");

export const searchDiscogsReleasesMutation = () =>
	createMutation(() => ({
		mutationFn: (input: { contributionId: string; query: string }) =>
			api.global.post<{ results: DiscogsReleaseCandidate[] }>(
				LibraryContributionApi.searchDiscogs(input.contributionId),
				{ query: input.query || null }
			),
		onError: (error) =>
			toastStore.show({ message: failureMessage("Couldn't search Discogs", error), type: 'error' })
	}));

export const selectDiscogsReleaseMutation = () =>
	createMutation(() => ({
		mutationFn: (input: {
			contributionId: string;
			expectedRowRevision: number;
			releaseIdOrUrl: string;
		}) =>
			api.global.post<LibraryContribution>(
				LibraryContributionApi.selectDiscogs(input.contributionId),
				{
					expected_row_revision: input.expectedRowRevision,
					release_id_or_url: input.releaseIdOrUrl
				}
			),
		onSuccess: async (contribution) => {
			await saveContribution(contribution);
			toastStore.show({ message: 'Discogs release selected', type: 'success' });
		},
		onError: async (error, input) =>
			refreshAfterMutationError(input.contributionId, "Couldn't select that Discogs release", error)
	}));

export const removeDiscogsReleaseMutation = () =>
	createMutation(() => ({
		mutationFn: (input: { contributionId: string; expectedRowRevision: number }) =>
			api.global.post<LibraryContribution>(
				LibraryContributionApi.removeDiscogs(input.contributionId),
				{
					expected_row_revision: input.expectedRowRevision
				}
			),
		onSuccess: async (contribution) => {
			await saveContribution(contribution);
			toastStore.show({ message: 'Discogs source removed', type: 'success' });
		},
		onError: async (error, input) =>
			refreshAfterMutationError(input.contributionId, "Couldn't remove the Discogs source", error)
	}));

export const checkMusicBrainzDuplicatesMutation = () =>
	createMutation(() => ({
		mutationFn: (input: {
			contributionId: string;
			expectedRowRevision: number;
			differentEditionConfirmed: boolean;
		}) =>
			api.global.post<LibraryContribution>(
				LibraryContributionApi.checkDuplicates(input.contributionId),
				{
					expected_row_revision: input.expectedRowRevision,
					different_edition_confirmed: input.differentEditionConfirmed
				}
			),
		onSuccess: async (contribution) => {
			await saveContribution(contribution);
			toastStore.show({ message: 'MusicBrainz check complete', type: 'success' });
		},
		onError: async (error, input) =>
			refreshAfterMutationError(
				input.contributionId,
				"Couldn't check MusicBrainz for duplicates",
				error
			)
	}));

export const attachExistingMusicBrainzReleaseMutation = () =>
	createMutation(() => ({
		mutationFn: (input: {
			contributionId: string;
			expectedRowRevision: number;
			releaseMbid: string;
		}) =>
			api.global.post<LibraryContribution>(
				LibraryContributionApi.attachRelease(input.contributionId),
				{
					expected_row_revision: input.expectedRowRevision,
					release_mbid: input.releaseMbid
				}
			),
		onSuccess: async (contribution) => {
			await saveContribution(contribution);
			await invalidateLibraryCatalog();
			toastStore.show({ message: 'Album linked to MusicBrainz', type: 'success' });
		},
		onError: async (error, input) =>
			refreshAfterMutationError(
				input.contributionId,
				"Couldn't link that MusicBrainz release",
				error
			)
	}));

export const createMusicBrainzSeedMutation = () =>
	createMutation(() => ({
		mutationFn: (input: { contributionId: string; expectedRowRevision: number }) =>
			api.global.post<MusicBrainzSeed>(LibraryContributionApi.seed(input.contributionId), {
				expected_row_revision: input.expectedRowRevision
			}),
		onSuccess: async (seed, input) => {
			await invalidateQueriesWithPersister({
				queryKey: LibraryContributionQueryKeyFactory.detail(
					authStore.user?.id,
					input.contributionId
				)
			});
		},
		onError: async (error, input) =>
			refreshAfterMutationError(input.contributionId, "Couldn't open the MusicBrainz editor", error)
	}));

export const recordMusicBrainzResultMutation = () =>
	createMutation(() => ({
		mutationFn: (input: {
			contributionId: string;
			expectedRowRevision: number;
			releaseIdOrUrl: string;
			replaceExistingResult: boolean;
		}) =>
			api.global.put<LibraryContribution>(
				LibraryContributionApi.recordResult(input.contributionId),
				{
					expected_row_revision: input.expectedRowRevision,
					release_id_or_url: input.releaseIdOrUrl,
					replace_existing_result: input.replaceExistingResult
				}
			),
		onSuccess: async (contribution) => {
			await saveContribution(contribution);
			toastStore.show({ message: 'MusicBrainz result queued for verification', type: 'success' });
		},
		onError: async (error, input) =>
			refreshAfterMutationError(
				input.contributionId,
				"Couldn't record that MusicBrainz release",
				error
			)
	}));

export const retryMusicBrainzVerificationMutation = () =>
	createMutation(() => ({
		mutationFn: (input: { contributionId: string; expectedRowRevision: number }) =>
			api.global.post<LibraryContribution>(
				LibraryContributionApi.retryVerification(input.contributionId),
				{ expected_row_revision: input.expectedRowRevision }
			),
		onSuccess: async (contribution) => {
			await saveContribution(contribution);
			toastStore.show({ message: 'Verification queued again', type: 'success' });
		},
		onError: async (error, input) =>
			refreshAfterMutationError(
				input.contributionId,
				"Couldn't retry MusicBrainz verification",
				error
			)
	}));
