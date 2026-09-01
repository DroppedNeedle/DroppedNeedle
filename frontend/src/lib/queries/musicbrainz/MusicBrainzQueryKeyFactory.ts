import { musicBrainzSourceKey } from './sourceScope.svelte';

export const MusicBrainzQueryKeyFactory = {
	prefix: ['musicbrainz'] as const,
	sourceScope: () => musicBrainzSourceKey(),
	settings: () =>
		[
			...MusicBrainzQueryKeyFactory.prefix,
			'settings',
			MusicBrainzQueryKeyFactory.sourceScope()
		] as const
};
