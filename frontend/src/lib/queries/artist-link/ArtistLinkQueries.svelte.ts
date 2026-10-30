import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import { v3 } from '$lib/api/v3/endpoint';
import type { components } from '$lib/api/v3/openapi';
import { applyEffects, artistLinkChanged, createAppMutation } from '$lib/queries/effects';
import { authStore } from '$lib/stores/authStore.svelte';
import { ArtistLinkQueryKeyFactory } from './ArtistLinkQueryKeyFactory';

export type ArtistLink = components['schemas']['ArtistLinkView'];

const linkUrl = (artistId: string) =>
	v3('/api/v3/library/artists/{id}/musicbrainz-link', { path: { id: artistId } });

const clearUrl = (artistId: string, expectedRevision: number) =>
	v3('/api/v3/library/artists/{id}/musicbrainz-link', {
		path: { id: artistId },
		query: { expected_revision: expectedRevision }
	});

export const getArtistLinkQuery = (artistId: Getter<string | undefined>) =>
	createQuery(() => {
		const artist = artistId();
		const user = authStore.user?.id;
		return {
			queryKey: ArtistLinkQueryKeyFactory.link(user, artist ?? ''),
			enabled: Boolean(user && artist),
			queryFn: ({ signal }: { signal: AbortSignal }) =>
				api.v3.GET(linkUrl(artist ?? ''), { signal })
		};
	});

// The pages under the old and the new MusicBrainz id both change, and so do
// the admin's duplicate groups.
const changed = (before: ArtistLink, after: ArtistLink) =>
	artistLinkChanged(
		authStore.user?.id,
		after.artist_id,
		...[before.musicbrainz_artist_id, after.musicbrainz_artist_id].filter((mbid): mbid is string =>
			Boolean(mbid)
		)
	);

// A refusal may come from a change made meanwhile: read the link again.
const refused = (link: ArtistLink) =>
	applyEffects(artistLinkChanged(authStore.user?.id, link.artist_id));

export function setArtistLink() {
	return createAppMutation(() => ({
		mutationFn: (input: { link: ArtistLink; musicbrainzArtistId: string }) =>
			api.v3.PUT(linkUrl(input.link.artist_id), {
				musicbrainz_artist_id: input.musicbrainzArtistId,
				expected_revision: input.link.artist_revision
			}),
		effects: (after, input) => [changed(input.link, after)],
		onError: (_error, input) => refused(input.link)
	}));
}

export function clearArtistLink() {
	return createAppMutation(() => ({
		mutationFn: (link: ArtistLink) => api.v3.DELETE(clearUrl(link.artist_id, link.artist_revision)),
		effects: (after, before) => [changed(before, after)],
		onError: (_error, link) => refused(link)
	}));
}
