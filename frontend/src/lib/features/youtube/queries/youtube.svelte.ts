import { SHARED, key } from '$lib/queries/keys';
import { v3 } from '$lib/api/v3/endpoint';
import { createQuery } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import type { V3Body } from '$lib/api/v3/client';
import type { YouTubeLink } from '$lib/features/youtube/queries/types';
import { setQueryDataWithPersister } from '$lib/queries/QueryClient';
import { albumVideoChanged, createAppMutation } from '$lib/queries/effects';

// Saved links are shared by everyone on the server.
export const YouTubeQueryKeyFactory = {
	feature: 'youtube',
	links: () => key(SHARED, 'youtube', 'links')
};

type GenerateAlbumBody = V3Body<'/api/v3/youtube/generate', 'post'>;
type GenerateTrackBody = V3Body<'/api/v3/youtube/generate-track', 'post'>;
type GenerateTracksBody = V3Body<'/api/v3/youtube/generate-tracks', 'post'>;
type ManualLinkBody = V3Body<'/api/v3/youtube/manual', 'post'>;
type UpdateLinkBody = V3Body<'/api/v3/youtube/link/{album_id}', 'put'>;

/** Every saved album link, for the YouTube library page. */
export const getYouTubeLinksQuery = () =>
	createQuery(() => ({
		queryKey: YouTubeQueryKeyFactory.links(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/youtube/links'), { signal })
	}));

function keepLink(link: YouTubeLink) {
	return setQueryDataWithPersister(YouTubeQueryKeyFactory.links(), (links?: YouTubeLink[]) => {
		if (!links) return links;
		const found = links.some((l) => l.album_id === link.album_id);
		return found ? links.map((l) => (l.album_id === link.album_id ? link : l)) : [link, ...links];
	});
}

/** Finds the album's video and saves it; spends YouTube quota. */
export const createGenerateAlbumYouTubeLinkMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (body: GenerateAlbumBody) => api.v3.POST(v3('/api/v3/youtube/generate'), body),
		onSuccess: (data) => keepLink(data.link)
	}));

/** Finds one track's video and saves it; spends YouTube quota. */
export const createGenerateYouTubeTrackLinkMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (body: GenerateTrackBody) => api.v3.POST(v3('/api/v3/youtube/generate-track'), body)
	}));

/** Finds the videos of the listed tracks; spends one quota unit per track. */
export const createGenerateYouTubeTrackLinksMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (body: GenerateTracksBody) =>
			api.v3.POST(v3('/api/v3/youtube/generate-tracks'), body)
	}));

/** Saves a link a person entered by hand. */
export const createManualYouTubeLinkMutation = () =>
	createAppMutation(() => ({
		mutationFn: (body: ManualLinkBody) => api.v3.POST(v3('/api/v3/youtube/manual'), body),
		onSuccess: (link) => keepLink(link),
		effects: (link) => [albumVideoChanged(link.album_id)]
	}));

/** Changes a saved link. */
export const createUpdateYouTubeLinkMutation = () =>
	createAppMutation(() => ({
		mutationFn: ({ albumId, body }: { albumId: string; body: UpdateLinkBody }) =>
			api.v3.PUT(v3('/api/v3/youtube/link/{album_id}', { path: { album_id: albumId } }), body),
		onSuccess: (link) => keepLink(link),
		effects: (link) => [albumVideoChanged(link.album_id)]
	}));

/** Removes an album's saved link. */
export const createDeleteYouTubeLinkMutation = () =>
	createAppMutation(() => ({
		mutationFn: (albumId: string) =>
			api.v3.DELETE(v3('/api/v3/youtube/link/{album_id}', { path: { album_id: albumId } })),
		onSuccess: async (_data, albumId) => {
			await setQueryDataWithPersister(YouTubeQueryKeyFactory.links(), (links?: YouTubeLink[]) =>
				links?.filter((l) => l.album_id !== albumId)
			);
		},
		effects: (_data, albumId) => [albumVideoChanged(albumId)]
	}));
