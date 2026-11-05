<!--
	The player's drawer: Queue, Lyrics and Equalizer as tabs of one panel on
	the right, full height above the player bar. The Lyrics tab is there
	only while the track has lyrics; a drawer left on it shows the queue
	until it has. One close button, named for the tab showing.
-->
<script lang="ts">
	import { X } from 'lucide-svelte';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import { deckFocus } from '$lib/features/player/state/deckFocus.svelte';
	import type { PlayerDrawerTab } from '$lib/features/player/state/playerDrawer.svelte';
	import { getLyricsQuery } from '$lib/features/player/queries/lyrics.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { navidromeFolderScope } from '$lib/features/remote-library/queries/RemoteQueries.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import SideDrawer from '$lib/ui/SideDrawer.svelte';
	import Tabs, { type TabItem } from '$lib/ui/Tabs.svelte';
	import QueueList from '$lib/features/player/components/QueueList.svelte';
	import LyricsView from '$lib/features/player/components/LyricsView.svelte';
	import EqView from '$lib/features/player/components/EqView.svelte';

	interface Props {
		open: boolean;
		tab: PlayerDrawerTab;
		onclose: () => void;
	}

	let { open = $bindable(), tab = $bindable(), onclose }: Props = $props();

	const lyricsQuery = getLyricsQuery(
		() => playerStore.nowPlaying,
		() => authStore.user?.id,
		() => navidromeFolderScope(authStore.user?.id)
	);
	const hasLyrics = $derived(lyricsQuery.isSuccess && lyricsQuery.data !== null);

	const shown = $derived<PlayerDrawerTab>(tab === 'lyrics' && !hasLyrics ? 'queue' : tab);
	const tabs = $derived<TabItem<PlayerDrawerTab>[]>([
		{ id: 'queue', label: 'Queue', count: playerStore.upcomingQueueLength || null },
		...(hasLyrics ? [{ id: 'lyrics' as const, label: 'Lyrics' }] : []),
		{ id: 'eq', label: 'Equalizer' }
	]);
	const names: Record<PlayerDrawerTab, string> = {
		queue: 'queue',
		lyrics: 'lyrics',
		eq: 'equalizer'
	};
	const titles: Record<PlayerDrawerTab, string> = {
		queue: 'Queue',
		lyrics: 'Lyrics',
		eq: 'Equalizer'
	};

	// Clear of the player bar while it shows; the Listening Room hides it behind its deck.
	const barShowing = $derived(playerStore.isPlayerVisible && !deckFocus.inView);
	const bottom = $derived(
		barShowing
			? 'calc(var(--ms-bottom-nav-offset) + var(--ms-player-height) + 0.5rem)'
			: 'calc(var(--ms-bottom-nav-offset) + 0.75rem)'
	);

	function close(): void {
		open = false;
		onclose();
	}
</script>

<SideDrawer
	bind:open
	label={titles[shown]}
	closeLabel="Close {names[shown]}"
	{bottom}
	onclose={() => onclose()}
>
	<Tabs
		{tabs}
		selected={shown}
		onselect={(id) => (tab = id)}
		label="Player"
		idPrefix="player-drawer"
		look="caps"
		class="w-full flex-nowrap border-b border-line pr-2 pl-5"
		panelClass="flex min-h-0 flex-1 flex-col"
	>
		{#snippet aside()}
			<IconButton size="sm" icon={X} label="Close {names[shown]}" class="ml-auto" onclick={close} />
		{/snippet}
		{#if shown === 'queue'}
			<QueueList oncleared={close} />
		{:else if shown === 'lyrics'}
			<LyricsView
				lyricsText={lyricsQuery.data?.text ?? ''}
				lines={lyricsQuery.data?.lines ?? []}
				isSynced={lyricsQuery.data?.is_synced ?? false}
				isLoading={lyricsQuery.isFetching}
				hasError={lyricsQuery.isError}
				currentTime={playerStore.progress}
				trackName={playerStore.nowPlaying?.trackName ?? ''}
				artistName={playerStore.nowPlaying?.artistName ?? ''}
			/>
		{:else}
			<div class="min-h-0 flex-1 overflow-y-auto">
				<EqView />
			</div>
		{/if}
	</Tabs>
</SideDrawer>
