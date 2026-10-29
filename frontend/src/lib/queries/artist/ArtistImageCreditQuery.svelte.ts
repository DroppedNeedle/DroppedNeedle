import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import { v3 } from '$lib/api/v3/endpoint';
import { STALE_SLOW } from '$lib/queries/staleTime';
import { ArtistQueryKeyFactory } from './ArtistQueryKeyFactory';

export const ARTIST_IMAGE_CREDIT_ENDPOINT = (mbid: string) =>
	v3('/api/v3/covers/artist/{artist_id}/credit', { path: { artist_id: mbid } });

export const getArtistImageCreditQuery = (getParams: Getter<{ mbid: string; enabled: boolean }>) =>
	createQuery(() => {
		const { mbid, enabled } = getParams();
		return {
			enabled: enabled && !!mbid,
			staleTime: STALE_SLOW,
			queryKey: ArtistQueryKeyFactory.imageCredit(mbid),
			queryFn: async ({ signal }) => {
				const answer = await api.v3.GET(ARTIST_IMAGE_CREDIT_ENDPOINT(mbid), { signal });
				return answer.credit;
			}
		};
	});
