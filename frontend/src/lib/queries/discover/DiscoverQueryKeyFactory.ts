import { musicBrainzSourceKey } from '../musicbrainz/sourceScope.svelte';

// Every personalized discover key carries a userId and provider source identity.
export const DiscoverQueryKeyFactory = {
	prefix: ['discover'] as const,
	discover: (userId: string | null | undefined) => {
		const normalizedUserId = userId ?? null;
		return [
			...DiscoverQueryKeyFactory.prefix,
			normalizedUserId,
			musicBrainzSourceKey(normalizedUserId)
		] as const;
	},
	radio: (userId: string | null | undefined, seedType: string, seedId: string) => {
		const normalizedUserId = userId ?? null;
		return [
			...DiscoverQueryKeyFactory.prefix,
			normalizedUserId,
			musicBrainzSourceKey(normalizedUserId),
			'radio',
			seedType,
			seedId
		] as const;
	},
	playlistSuggestions: (userId: string | null | undefined, playlistId: string) => {
		const normalizedUserId = userId ?? null;
		return [
			...DiscoverQueryKeyFactory.prefix,
			normalizedUserId,
			musicBrainzSourceKey(normalizedUserId),
			'playlist-suggestions',
			playlistId
		] as const;
	}
};
