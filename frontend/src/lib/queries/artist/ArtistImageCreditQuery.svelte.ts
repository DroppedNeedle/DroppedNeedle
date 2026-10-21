import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import { v3 } from '$lib/api/v3/endpoint';
import { CACHE_TTL } from '$lib/constants';
import { ArtistQueryKeyFactory } from './ArtistQueryKeyFactory';

// The credit is stored with the artist's picture, so it is read once the
// picture has loaded and lives as long as the artist's other page data.
export const ArtistImageCreditQueryKeyFactory = {
	credit: (mbid: string) => [...ArtistQueryKeyFactory.prefix, 'image-credit', mbid] as const
};

export const ARTIST_IMAGE_CREDIT_ENDPOINT = (mbid: string) =>
	v3('/api/v3/covers/artist/{artist_id}/credit', { path: { artist_id: mbid } });

export const getArtistImageCreditQuery = (getParams: Getter<{ mbid: string; enabled: boolean }>) =>
	createQuery(() => {
		const { mbid, enabled } = getParams();
		return {
			enabled: enabled && !!mbid,
			staleTime: CACHE_TTL.ARTIST_DETAIL_EXTENDED,
			queryKey: ArtistImageCreditQueryKeyFactory.credit(mbid),
			queryFn: async ({ signal }) => {
				const answer = await api.global.v3.GET(ARTIST_IMAGE_CREDIT_ENDPOINT(mbid), { signal });
				return answer.credit;
			}
		};
	});
