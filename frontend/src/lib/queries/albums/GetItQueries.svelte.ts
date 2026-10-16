import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import { CATALOG_ENDPOINTS } from '$lib/queries/catalog/endpoints';

// "Where to buy" links. Deliberately lazy: only the inline
// section fetches this, with its own skeleton - the album page's load path
// never pays for the cold MB/iTunes lookups. Not user-dependent (links come
// from global admin settings), so no userId key segment.
export const purchaseOptionsKey = (mbid: string) =>
	['albums', 'purchase-options', 'v2', mbid] as const;

export const getPurchaseOptionsQuery = (
	mbid: Getter<string>,
	enabled: Getter<boolean> = () => true
) =>
	createQuery(() => ({
		queryKey: purchaseOptionsKey(mbid()),
		enabled: enabled() && !!mbid(),
		staleTime: 24 * 60 * 60 * 1000, // the backend caches for 7 days anyway
		queryFn: ({ signal }) =>
			api.global.v3.GET(CATALOG_ENDPOINTS.albumPurchaseOptions(mbid()), { signal })
	}));

export const artistPurchaseOptionsKey = (mbid: string) =>
	['artists', 'purchase-options', 'v2', mbid] as const;

// The artist's own storefronts (their Bandcamp page, merch shop) - not one album.
export const getArtistPurchaseOptionsQuery = (mbid: Getter<string>, name: Getter<string>) =>
	createQuery(() => ({
		queryKey: artistPurchaseOptionsKey(mbid()),
		enabled: !!mbid(),
		staleTime: 24 * 60 * 60 * 1000,
		queryFn: ({ signal }) =>
			api.global.v3.GET(CATALOG_ENDPOINTS.artistPurchaseOptions(mbid(), name()), { signal })
	}));

// Notes, tags and links metadata plugins add about one album. Lazy like the
// purchase links; empty when no plugin knows the album.
export const pluginDetailsKey = (mbid: string) => ['albums', 'plugin-details', mbid] as const;

export const getAlbumPluginDetailsQuery = (mbid: Getter<string>) =>
	createQuery(() => ({
		queryKey: pluginDetailsKey(mbid()),
		enabled: !!mbid(),
		staleTime: 60 * 60 * 1000,
		queryFn: ({ signal }) =>
			api.global.v3.GET(CATALOG_ENDPOINTS.albumPluginDetails(mbid()), { signal })
	}));
