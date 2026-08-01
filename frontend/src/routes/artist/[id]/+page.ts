import type { PageLoad } from './$types';

export const load: PageLoad = async ({ params, url }) => {
	return {
		artistId: params.id,
		// ?view=discography forces the provider (MusicBrainz) page even when the
		// artist exists locally, so owning one album doesn't hide the discography
		discographyView: url.searchParams.get('view') === 'discography'
	};
};
