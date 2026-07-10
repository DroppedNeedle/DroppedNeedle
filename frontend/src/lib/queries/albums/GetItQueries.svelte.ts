import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import type { ArtistPurchaseOptionsResponse, PurchaseOptionsResponse } from '$lib/types';

// "Where to buy" (Get it, phase 01). Deliberately lazy: only the inline
// section fetches this, with its own skeleton - the album page's load path
// never pays for the cold MB/iTunes lookups. Not user-dependent (links come
// from global admin settings), so no userId key segment.
export const purchaseOptionsKey = (mbid: string) => ['albums', 'purchase-options', mbid] as const;

export const getPurchaseOptionsQuery = (mbid: Getter<string>) =>
	createQuery(() => ({
		queryKey: purchaseOptionsKey(mbid()),
		enabled: !!mbid(),
		staleTime: 24 * 60 * 60 * 1000, // the backend caches for 7 days anyway
		queryFn: ({ signal }) =>
			api.global.get<PurchaseOptionsResponse>(API.album.purchaseOptions(mbid()), { signal })
	}));

export const artistPurchaseOptionsKey = (mbid: string) =>
	['artists', 'purchase-options', mbid] as const;

// The artist's own storefronts (their Bandcamp page, merch shop) - not one album.
export const getArtistPurchaseOptionsQuery = (mbid: Getter<string>, name: Getter<string>) =>
	createQuery(() => ({
		queryKey: artistPurchaseOptionsKey(mbid()),
		enabled: !!mbid(),
		staleTime: 24 * 60 * 60 * 1000,
		queryFn: ({ signal }) =>
			api.global.get<ArtistPurchaseOptionsResponse>(API.artist.purchaseOptions(mbid(), name()), {
				signal
			})
	}));
