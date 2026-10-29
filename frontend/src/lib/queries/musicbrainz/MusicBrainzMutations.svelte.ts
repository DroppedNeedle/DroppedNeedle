import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { setQueryDataWithPersister } from '$lib/queries/QueryClient';
import { setMusicBrainzSourceScope } from './sourceScope.svelte';

import { MusicBrainzQueryKeyFactory } from './MusicBrainzQueryKeyFactory';
import { MUSICBRAINZ_ENDPOINTS } from './endpoints';
import { toMusicBrainzSettings } from './MusicBrainzAdapters';
import type {
	BrainzMashBinding,
	MusicBrainzSettingsResponse,
	MusicBrainzSettingsUpdate
} from './types';
import { createAppMutation } from '$lib/queries/effects';

interface MusicBrainzMutationContext {
	userId: string | null;
}

function isMusicBrainzMutationContext(value: unknown): value is MusicBrainzMutationContext {
	if (typeof value !== 'object' || value === null) return false;
	const candidate = value as Record<string, unknown>;
	return (
		'userId' in candidate && (candidate.userId === null || typeof candidate.userId === 'string')
	);
}

function captureMusicBrainzUser(): MusicBrainzMutationContext {
	return { userId: authStore.user?.id ?? null };
}

const cacheSettingsResponse = async (
	data: MusicBrainzSettingsResponse,
	_variables: unknown,
	onMutateResult: unknown
) => {
	if (
		!isMusicBrainzMutationContext(onMutateResult) ||
		(authStore.user?.id ?? null) !== onMutateResult.userId
	) {
		return;
	}
	setMusicBrainzSourceScope(data, onMutateResult.userId);
	try {
		await setQueryDataWithPersister(
			MusicBrainzQueryKeyFactory.settings(onMutateResult.userId),
			data
		);
	} catch {
		// Persisted settings are a cache optimization; a committed source response
		// must still update the in-memory scope and let the caller finish its sweep.
	}
};

export const saveMusicBrainzSettings = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: async (settings: MusicBrainzSettingsUpdate) =>
			toMusicBrainzSettings(await api.v3.PUT(MUSICBRAINZ_ENDPOINTS.settings(), settings)),
		onMutate: captureMusicBrainzUser,
		onSuccess: cacheSettingsResponse
	}));

export const consentBrainzMash = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: async (binding: BrainzMashBinding) =>
			toMusicBrainzSettings(await api.v3.POST(MUSICBRAINZ_ENDPOINTS.brainzMashConsent(), binding)),
		onMutate: captureMusicBrainzUser,
		onSuccess: cacheSettingsResponse
	}));

export const testMusicBrainzConnection = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: async (request: BrainzMashBinding | MusicBrainzSettingsUpdate) =>
			toMusicBrainzSettings(await api.v3.POST(MUSICBRAINZ_ENDPOINTS.verify(), request)),
		onMutate: captureMusicBrainzUser,
		onSuccess: cacheSettingsResponse
	}));

export const stageBrainzMash = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: async () =>
			toMusicBrainzSettings(await api.v3.POST(MUSICBRAINZ_ENDPOINTS.brainzMashStage())),
		onMutate: captureMusicBrainzUser,
		onSuccess: cacheSettingsResponse
	}));

export const activateBrainzMash = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: async (binding: BrainzMashBinding) =>
			toMusicBrainzSettings(await api.v3.POST(MUSICBRAINZ_ENDPOINTS.activate(), binding)),
		onMutate: captureMusicBrainzUser,
		onSuccess: cacheSettingsResponse
	}));
