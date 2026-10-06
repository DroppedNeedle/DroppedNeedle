import { createMutation } from '@tanstack/svelte-query';

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
		await setQueryDataWithPersister(MusicBrainzQueryKeyFactory.settings(), data);
	} catch {
		// Persisted settings are a cache optimization; a committed source response
		// must still update the in-memory scope and let the caller finish its sweep.
	}
};

export const saveMusicBrainzSettings = () =>
	createMutation(() => ({
		mutationFn: async (settings: MusicBrainzSettingsUpdate) =>
			toMusicBrainzSettings(await api.global.v3.PUT(MUSICBRAINZ_ENDPOINTS.settings(), settings)),
		onMutate: captureMusicBrainzUser,
		onSuccess: cacheSettingsResponse
	}));

export const consentBrainzMash = () =>
	createMutation(() => ({
		mutationFn: async (binding: BrainzMashBinding) =>
			toMusicBrainzSettings(
				await api.global.v3.POST(MUSICBRAINZ_ENDPOINTS.brainzMashConsent(), binding)
			),
		onMutate: captureMusicBrainzUser,
		onSuccess: cacheSettingsResponse
	}));

export const testMusicBrainzConnection = () =>
	createMutation(() => ({
		mutationFn: async (request: BrainzMashBinding | MusicBrainzSettingsUpdate) =>
			toMusicBrainzSettings(await api.global.v3.POST(MUSICBRAINZ_ENDPOINTS.verify(), request)),
		onMutate: captureMusicBrainzUser,
		onSuccess: cacheSettingsResponse
	}));

export const stageBrainzMash = () =>
	createMutation(() => ({
		mutationFn: async () =>
			toMusicBrainzSettings(await api.global.v3.POST(MUSICBRAINZ_ENDPOINTS.brainzMashStage())),
		onMutate: captureMusicBrainzUser,
		onSuccess: cacheSettingsResponse
	}));

export const activateBrainzMash = () =>
	createMutation(() => ({
		mutationFn: async (binding: BrainzMashBinding) =>
			toMusicBrainzSettings(await api.global.v3.POST(MUSICBRAINZ_ENDPOINTS.activate(), binding)),
		onMutate: captureMusicBrainzUser,
		onSuccess: cacheSettingsResponse
	}));
