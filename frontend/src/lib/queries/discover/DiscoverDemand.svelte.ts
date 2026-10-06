import { createMutation } from '@tanstack/svelte-query';
import { untrack } from 'svelte';
import { api } from '$lib/api/client';
import {
	musicBrainzSourceKey,
	setMusicBrainzSourceScope
} from '$lib/queries/musicbrainz/sourceScope.svelte';
import type { MusicBrainzSourceMode } from '$lib/queries/musicbrainz/types';
import { authStore } from '$lib/stores/authStore.svelte';
import type { DiscoverActivity } from '$lib/types';
import { DiscoverV3Api } from './DiscoverV3Api';

// Discover demand: the server records which discovery surfaces a user
// actually looks at, and answers with the MusicBrainz source the user's
// provider data is scoped to.
const KNOWN_SOURCE_MODES: MusicBrainzSourceMode[] = [
	'brainzmash',
	'official',
	'mirror',
	'community'
];

export async function recordDiscoverActivity(
	activity: DiscoverActivity,
	signal?: AbortSignal
): Promise<void> {
	const userId = authStore.user?.id;
	const before = JSON.stringify(musicBrainzSourceKey());
	const source = await api.global.v3.POST(DiscoverV3Api.activity(), activity, { signal });
	const knownMode = KNOWN_SOURCE_MODES.find((mode) => mode === source.source_mode);
	if (
		!signal?.aborted &&
		userId === authStore.user?.id &&
		before === JSON.stringify(musicBrainzSourceKey()) &&
		knownMode !== undefined
	) {
		setMusicBrainzSourceScope(
			{ source_mode: knownMode, source_id: source.source_id, generation: source.generation },
			userId
		);
	}
}

export function useDiscoverActivity(
	getActivity: () => DiscoverActivity | null,
	getElement?: () => HTMLElement | undefined
): void {
	const mutation = createMutation(() => ({
		retry: false,
		mutationFn: (activity: DiscoverActivity) => recordDiscoverActivity(activity)
	}));
	$effect(() => {
		const userId = authStore.user?.id;
		const activity = getActivity();
		const element = getElement?.();
		if (!userId || !activity || (getElement && !element)) return;
		let visible = !getElement;
		let entered = false;
		const signalEntry = () => {
			const active = visible && document.visibilityState === 'visible';
			if (active && !entered) untrack(() => mutation.mutate(activity));
			entered = active;
		};
		const onFocus = () => {
			entered = false;
			signalEntry();
		};
		const observer = element
			? new IntersectionObserver(([entry]) => {
					visible = entry.isIntersecting;
					signalEntry();
				})
			: null;
		if (element) observer?.observe(element);
		document.addEventListener('visibilitychange', signalEntry);
		window.addEventListener('focus', onFocus);
		signalEntry();
		return () => {
			observer?.disconnect();
			document.removeEventListener('visibilitychange', signalEntry);
			window.removeEventListener('focus', onFocus);
		};
	});
}
