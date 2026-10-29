import { createMutation, createQuery } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { V3Body } from '$lib/api/v3/client';
import type { YouTubeLink } from '$lib/types';
import { AlbumQueryKeyFactory } from '../catalog/AlbumQueryKeyFactory';
import { invalidateQueriesWithPersister, setQueryDataWithPersister } from '../QueryClient';
import { YOUTUBE_ENDPOINTS } from './endpoints';

// Saved links are shared by everyone on the server, so the keys carry no user.
export const YouTubeQueryKeyFactory = {
	prefix: ['youtube'] as const,
	links: () => [...YouTubeQueryKeyFactory.prefix, 'links'] as const
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
		queryFn: ({ signal }) => api.v3.GET(YOUTUBE_ENDPOINTS.links(), { signal })
	}));

function keepLink(link: YouTubeLink) {
	return setQueryDataWithPersister(YouTubeQueryKeyFactory.links(), (links?: YouTubeLink[]) => {
		if (!links) return links;
		const found = links.some((l) => l.album_id === link.album_id);
		return found ? links.map((l) => (l.album_id === link.album_id ? link : l)) : [link, ...links];
	});
}

function refreshAlbumLinks(albumId: string) {
	return invalidateQueriesWithPersister({ queryKey: AlbumQueryKeyFactory.youtubeLink(albumId) });
}

/** Finds the album's video and saves it; spends YouTube quota. */
export const createGenerateAlbumYouTubeLinkMutation = () =>
	createMutation(() => ({
		mutationFn: (body: GenerateAlbumBody) => api.v3.POST(YOUTUBE_ENDPOINTS.generate(), body),
		onSuccess: (data) => keepLink(data.link)
	}));

/** Finds one track's video and saves it; spends YouTube quota. */
export const createGenerateYouTubeTrackLinkMutation = () =>
	createMutation(() => ({
		mutationFn: (body: GenerateTrackBody) => api.v3.POST(YOUTUBE_ENDPOINTS.generateTrack(), body)
	}));

/** Finds the videos of the listed tracks; spends one quota unit per track. */
export const createGenerateYouTubeTrackLinksMutation = () =>
	createMutation(() => ({
		mutationFn: (body: GenerateTracksBody) => api.v3.POST(YOUTUBE_ENDPOINTS.generateTracks(), body)
	}));

/** Saves a link a person entered by hand. */
export const createManualYouTubeLinkMutation = () =>
	createMutation(() => ({
		mutationFn: (body: ManualLinkBody) => api.v3.POST(YOUTUBE_ENDPOINTS.manual(), body),
		onSuccess: async (link) => {
			await keepLink(link);
			await refreshAlbumLinks(link.album_id);
		}
	}));

/** Changes a saved link. */
export const createUpdateYouTubeLinkMutation = () =>
	createMutation(() => ({
		mutationFn: ({ albumId, body }: { albumId: string; body: UpdateLinkBody }) =>
			api.v3.PUT(YOUTUBE_ENDPOINTS.link(albumId), body),
		onSuccess: async (link) => {
			await keepLink(link);
			await refreshAlbumLinks(link.album_id);
		}
	}));

/** Removes an album's saved link. */
export const createDeleteYouTubeLinkMutation = () =>
	createMutation(() => ({
		mutationFn: (albumId: string) => api.v3.DELETE(YOUTUBE_ENDPOINTS.link(albumId)),
		onSuccess: async (_data, albumId) => {
			await setQueryDataWithPersister(YouTubeQueryKeyFactory.links(), (links?: YouTubeLink[]) =>
				links?.filter((l) => l.album_id !== albumId)
			);
			await refreshAlbumLinks(albumId);
		}
	}));
