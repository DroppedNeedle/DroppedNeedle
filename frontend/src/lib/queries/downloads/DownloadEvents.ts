import { authStore } from '$lib/stores/authStore.svelte';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import {
	muxEventStream,
	type MuxEventStream,
	type MuxUnsubscribe
} from '$lib/queries/events/MuxEventStream';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';

// `downloads.changed` says the download queue moved somewhere. The event carries
// no task details, so the tab refetches its own queue summary; the nav badge then
// compares the summary revision and refreshes the full list only when this user's
// queue actually changed. A (re)connect may have missed events, so it refetches too.
export function createDownloadEvents(mux: MuxEventStream = muxEventStream) {
	let unsubs: MuxUnsubscribe[] = [];

	function refreshSummary(): void {
		const userId = authStore.user?.id;
		if (!userId) return;
		void invalidateQueriesWithPersister({
			queryKey: DownloadQueryKeyFactory.activity(userId),
			exact: true
		});
	}

	function start(): void {
		stop();
		unsubs = [mux.on('downloads.changed', refreshSummary), mux.onConnect(refreshSummary)];
	}

	function stop(): void {
		for (const unsub of unsubs) unsub();
		unsubs = [];
	}

	return { start, stop };
}
