import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import type { HeldListResponse } from '$lib/types';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import { DOWNLOAD_HELD_ENDPOINTS } from './endpoints';

// Tracks held for an "import anyway" review. Two callers: the Downloads dashboard (no
// filter, cross-album triage) and the album page (scoped to one release group). Keyed under
// tasks() so a download mutation's invalidateTasks() refreshes it too. The global activity
// revision invalidates this prefix after held rows change, so it needs no parallel timer -
// and no focus-'always' refetch (B6): invalidations mark the key stale, so a tab return
// still refetches promptly after real changes; inside the 30 s window it costs nothing.
export const getHeldImportsQuery = (
	getMbid: Getter<string | undefined> = () => undefined,
	getEnabled: Getter<boolean> = () => true
) =>
	createQuery(() => ({
		enabled: getEnabled(),
		staleTime: 30_000,
		queryKey: DownloadQueryKeyFactory.held(authStore.user?.id, getMbid()),
		queryFn: ({ signal }) =>
			api.global.get<HeldListResponse>(DOWNLOAD_HELD_ENDPOINTS.list(getMbid()), { signal })
	}));
