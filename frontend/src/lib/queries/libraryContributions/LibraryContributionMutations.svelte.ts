import { resolve } from '$app/paths';
import { createMutation } from '@tanstack/svelte-query';
import { goto } from '$app/navigation';
import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import type {
	DiscogsReleaseCandidate,
	LibraryContribution,
	MusicBrainzSeed,
	ReleaseDraft
} from '$lib/types';
import { LibraryQueryKeyFactory } from '$lib/queries/library/LibraryQueryKeyFactory';
import { invalidateLibraryCatalog } from '$lib/queries/library/LibraryCatalogInvalidation';
import {
	invalidateQueriesWithPersister,
	setQueryDataWithPersister
} from '$lib/queries/QueryClient';
import { LibraryContributionQueryKeyFactory } from './LibraryContributionQueryKeyFactory';

const saveContribution = async (contribution: LibraryContribution): Promise<void> => {
	await setQueryDataWithPersister(
		LibraryContributionQueryKeyFactory.detail(authStore.user?.id, contribution.id),
		contribution
	);
	await invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.all });
};

const refreshAfterMutationError = async (
	contributionId: string,
	message: string
): Promise<void> => {
	await invalidateQueriesWithPersister({
		queryKey: LibraryContributionQueryKeyFactory.detail(authStore.user?.id, contributionId)
	});
	toastStore.show({ message, type: 'error' });
};

export const createLibraryContributionMutation = () =>
	createMutation(() => ({
		mutationFn: (albumId: string) =>
			api.global.post<LibraryContribution>(API.library.createContribution(albumId), {}),
		onSuccess: async (contribution) => {
			await saveContribution(contribution);
			toastStore.show({ message: 'Contribution draft ready', type: 'success' });
			await goto(resolve(`/library/contributions/${contribution.id}`));
		},
		onError: () => toastStore.show({ message: "Couldn't start the contribution", type: 'error' })
	}));

export const updateLibraryContributionMutation = () =>
	createMutation(() => ({
		mutationFn: (input: {
			contributionId: string;
			expectedRowRevision: number;
			draft: ReleaseDraft;
		}) =>
			api.global.put<LibraryContribution>(API.library.contributionDraft(input.contributionId), {
				expected_row_revision: input.expectedRowRevision,
				draft: input.draft
			}),
		onSuccess: async (contribution) => {
			await saveContribution(contribution);
			toastStore.show({ message: 'Draft saved', type: 'success' });
		},
		onError: async (_error, input) =>
			refreshAfterMutationError(input.contributionId, "Couldn't save the draft")
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
					? API.library.rebuildContribution(input.contributionId)
					: API.library.cancelContribution(input.contributionId);
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
				await goto(resolve(`/library/contributions/${contribution.id}`), { replaceState: true });
			}
		},
		onError: async (_error, input) => refreshAfterMutationError(input.contributionId, errorMessage)
	}));

export const rebuildLibraryContributionMutation = () =>
	revisionMutation('rebuild', 'Draft rebuilt from the current album', "Couldn't rebuild the draft");

export const cancelLibraryContributionMutation = () =>
	revisionMutation('cancel', 'Contribution cancelled', "Couldn't cancel the contribution");

export const searchDiscogsReleasesMutation = () =>
	createMutation(() => ({
		mutationFn: (input: { contributionId: string; query: string }) =>
			api.global.post<{ results: DiscogsReleaseCandidate[] }>(
				API.library.searchDiscogsReleases(input.contributionId),
				{ query: input.query || null }
			),
		onError: () => toastStore.show({ message: "Couldn't search Discogs", type: 'error' })
	}));

export const selectDiscogsReleaseMutation = () =>
	createMutation(() => ({
		mutationFn: (input: {
			contributionId: string;
			expectedRowRevision: number;
			releaseIdOrUrl: string;
		}) =>
			api.global.post<LibraryContribution>(API.library.selectDiscogsRelease(input.contributionId), {
				expected_row_revision: input.expectedRowRevision,
				release_id_or_url: input.releaseIdOrUrl
			}),
		onSuccess: async (contribution) => {
			await saveContribution(contribution);
			toastStore.show({ message: 'Discogs release selected', type: 'success' });
		},
		onError: async (_error, input) =>
			refreshAfterMutationError(input.contributionId, "Couldn't select that Discogs release")
	}));

export const removeDiscogsReleaseMutation = () =>
	createMutation(() => ({
		mutationFn: (input: { contributionId: string; expectedRowRevision: number }) =>
			api.global.post<LibraryContribution>(API.library.removeDiscogsRelease(input.contributionId), {
				expected_row_revision: input.expectedRowRevision
			}),
		onSuccess: async (contribution) => {
			await saveContribution(contribution);
			toastStore.show({ message: 'Discogs source removed', type: 'success' });
		},
		onError: async (_error, input) =>
			refreshAfterMutationError(input.contributionId, "Couldn't remove the Discogs source")
	}));

export const checkMusicBrainzDuplicatesMutation = () =>
	createMutation(() => ({
		mutationFn: (input: {
			contributionId: string;
			expectedRowRevision: number;
			differentEditionConfirmed: boolean;
		}) =>
			api.global.post<LibraryContribution>(
				API.library.checkContributionDuplicates(input.contributionId),
				{
					expected_row_revision: input.expectedRowRevision,
					different_edition_confirmed: input.differentEditionConfirmed
				}
			),
		onSuccess: async (contribution) => {
			await saveContribution(contribution);
			toastStore.show({ message: 'MusicBrainz check complete', type: 'success' });
		},
		onError: async (_error, input) =>
			refreshAfterMutationError(input.contributionId, "Couldn't check MusicBrainz for duplicates")
	}));

export const attachExistingMusicBrainzReleaseMutation = () =>
	createMutation(() => ({
		mutationFn: (input: {
			contributionId: string;
			expectedRowRevision: number;
			releaseMbid: string;
		}) =>
			api.global.post<LibraryContribution>(
				API.library.attachContributionRelease(input.contributionId),
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
		onError: async (_error, input) =>
			refreshAfterMutationError(input.contributionId, "Couldn't link that MusicBrainz release")
	}));

export const createMusicBrainzSeedMutation = () =>
	createMutation(() => ({
		mutationFn: (input: { contributionId: string; expectedRowRevision: number }) =>
			api.global.post<MusicBrainzSeed>(API.library.createContributionSeed(input.contributionId), {
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
		onError: async (_error, input) =>
			refreshAfterMutationError(input.contributionId, "Couldn't open the MusicBrainz editor")
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
				API.library.recordContributionResult(input.contributionId),
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
		onError: async (_error, input) =>
			refreshAfterMutationError(input.contributionId, "Couldn't record that MusicBrainz release")
	}));

export const retryMusicBrainzVerificationMutation = () =>
	createMutation(() => ({
		mutationFn: (input: { contributionId: string; expectedRowRevision: number }) =>
			api.global.post<LibraryContribution>(
				API.library.retryContributionVerification(input.contributionId),
				{ expected_row_revision: input.expectedRowRevision }
			),
		onSuccess: async (contribution) => {
			await saveContribution(contribution);
			toastStore.show({ message: 'Verification queued again', type: 'success' });
		},
		onError: async (_error, input) =>
			refreshAfterMutationError(input.contributionId, "Couldn't retry MusicBrainz verification")
	}));
