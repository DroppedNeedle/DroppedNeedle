import { browser } from '$app/environment';

import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { activeCount, isActiveDownloadStatus } from '$lib/queries/downloads/downloadStatus';
import { LibraryQueryKeyFactory } from '$lib/queries/library/LibraryQueryKeyFactory';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { libraryStore } from '$lib/stores/library';
import type { DownloadListResponse, DownloadStatus } from '$lib/types';

// nav-badge active-downloads count; light 10s poll so background downloads show
// anywhere (the /downloads page polls faster). best-effort, transient errors ignored
let count = $state(0);
let timer: ReturnType<typeof setInterval> | null = null;
let started = false;
// last-seen status per task, so a download that finishes anywhere in the app flips
// its album to In-Library (and refreshes library views) without a manual reload
let prevStatus = new Map<string, DownloadStatus>();

async function poll(): Promise<void> {
	try {
		const res = await api.global.get<DownloadListResponse>(API.downloads.list(undefined, 1, 100));
		const items = res.items;
		count = activeCount(items);

		let landed = false;
		for (const t of items) {
			const prev = prevStatus.get(t.id);
			const wasActive = prev !== undefined && isActiveDownloadStatus(prev);
			if (wasActive && (t.status === 'completed' || t.status === 'partial') && t.release_group_mbid) {
				libraryStore.addMbid(t.release_group_mbid);
				landed = true;
			}
		}
		prevStatus = new Map(items.map((t) => [t.id, t.status]));
		if (landed) {
			void invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.all });
		}
	} catch {
		// non-critical; leave the last known count in place
	}
}

export const downloadsActivity = {
	get count() {
		return count;
	},
	get isActive() {
		return count > 0;
	},
	start(): void {
		if (!browser || started) return;
		started = true;
		void poll();
		timer = setInterval(() => void poll(), 10000);
	},
	refresh(): void {
		void poll();
	},
	stop(): void {
		if (timer) clearInterval(timer);
		timer = null;
		started = false;
		count = 0;
	}
};
