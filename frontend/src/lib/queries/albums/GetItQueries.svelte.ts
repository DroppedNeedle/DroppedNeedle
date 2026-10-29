import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import { CATALOG_ENDPOINTS } from '$lib/queries/catalog/endpoints';
import { AlbumQueryKeyFactory } from '$lib/queries/catalog/AlbumQueryKeyFactory';
import { ArtistQueryKeyFactory } from '$lib/queries/artist/ArtistQueryKeyFactory';
import { STALE_SLOW } from '$lib/queries/staleTime';

// "Where to buy" links. Deliberately lazy: only the inline
// section fetches this, with its own skeleton - the album page's load path
// never pays for the cold MB/iTunes lookups. The same for everyone (links
// come from global admin settings).
export const getPurchaseOptionsQuery = (
	mbid: Getter<string>,
	enabled: Getter<boolean> = () => true
) =>
	createQuery(() => ({
		queryKey: AlbumQueryKeyFactory.purchaseOptions(mbid()),
		enabled: enabled() && !!mbid(),
		staleTime: STALE_SLOW,
		queryFn: ({ signal }) => api.v3.GET(CATALOG_ENDPOINTS.albumPurchaseOptions(mbid()), { signal })
	}));

// The artist's own storefronts (their Bandcamp page, merch shop) - not one album.
export const getArtistPurchaseOptionsQuery = (mbid: Getter<string>, name: Getter<string>) =>
	createQuery(() => ({
		queryKey: ArtistQueryKeyFactory.purchaseOptions(mbid()),
		enabled: !!mbid(),
		staleTime: STALE_SLOW,
		queryFn: ({ signal }) =>
			api.v3.GET(CATALOG_ENDPOINTS.artistPurchaseOptions(mbid(), name()), { signal })
	}));

// Notes, tags and links metadata plugins add about one album. Lazy like the
// purchase links; empty when no plugin knows the album.
export const getAlbumPluginDetailsQuery = (mbid: Getter<string>) =>
	createQuery(() => ({
		queryKey: AlbumQueryKeyFactory.pluginDetails(mbid()),
		enabled: !!mbid(),
		staleTime: STALE_SLOW,
		queryFn: ({ signal }) => api.v3.GET(CATALOG_ENDPOINTS.albumPluginDetails(mbid()), { signal })
	}));
