import { v3 } from '$lib/api/v3/endpoint';

// /api/v3 contribution URLs, built through the typed registry: every
// template is a literal the contract-coverage gate checks against the
// generated spec.
const byId = (id: string) => ({ path: { id } });

export const LibraryContributionApi = {
	create: (albumId: string) => v3('/api/v3/library/albums/{id}/contributions', byId(albumId)),
	detail: (id: string) => v3('/api/v3/library/contributions/{id}', byId(id)),
	draft: (id: string) => v3('/api/v3/library/contributions/{id}/draft', byId(id)),
	rebuild: (id: string) => v3('/api/v3/library/contributions/{id}/rebuild', byId(id)),
	cancel: (id: string) => v3('/api/v3/library/contributions/{id}/cancel', byId(id)),
	searchDiscogs: (id: string) => v3('/api/v3/library/contributions/{id}/discogs/search', byId(id)),
	selectDiscogs: (id: string) => v3('/api/v3/library/contributions/{id}/discogs/select', byId(id)),
	removeDiscogs: (id: string) => v3('/api/v3/library/contributions/{id}/discogs/remove', byId(id)),
	checkDuplicates: (id: string) =>
		v3('/api/v3/library/contributions/{id}/musicbrainz/duplicates', byId(id)),
	attachRelease: (id: string) =>
		v3('/api/v3/library/contributions/{id}/musicbrainz/attach', byId(id)),
	seed: (id: string) => v3('/api/v3/library/contributions/{id}/musicbrainz/seed', byId(id)),
	recordResult: (id: string) =>
		v3('/api/v3/library/contributions/{id}/musicbrainz/result', byId(id)),
	retryVerification: (id: string) =>
		v3('/api/v3/library/contributions/{id}/musicbrainz/verify', byId(id))
};
