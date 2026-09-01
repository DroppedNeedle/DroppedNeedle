import { musicBrainzSourceKey } from './musicbrainz/sourceScope.svelte';

export const HomeQueryKeyFactory = {
	prefix: ['home'] as const,
	// userId and the active MusicBrainz source identity isolate persisted provider data.
	home: (userId: string | null | undefined) => {
		const normalizedUserId = userId ?? null;
		return [
			...HomeQueryKeyFactory.prefix,
			normalizedUserId,
			musicBrainzSourceKey(normalizedUserId)
		] as const;
	},
	integrationStatus: (userId: string | null | undefined) =>
		[...HomeQueryKeyFactory.prefix, userId ?? null, 'integration-status'] as const
};
