<script lang="ts">
	import type { Snippet } from 'svelte';
	import { Shuffle, Play, ListPlus, ListStart, ListMusic, Download } from 'lucide-svelte';
	import ContextMenu from '$lib/ui/ContextMenu.svelte';
	import type { MenuItem } from '$lib/ui/ContextMenu.svelte';
	import Button from '$lib/ui/Button.svelte';

	interface Props {
		sourceLabel: string;
		sourceColor: string;
		trackCount: number;
		totalTracks: number;
		extraBadge?: string | null;
		onPlayAll: () => void;
		onShuffle: () => void;
		onAddAllToQueue?: () => void;
		onPlayAllNext?: () => void;
		onAddAllToPlaylist?: () => void;
		onDownload?: () => void;
		icon: Snippet;
	}

	let {
		sourceLabel,
		sourceColor,
		trackCount,
		totalTracks,
		extraBadge = null,
		onPlayAll,
		onShuffle,
		onAddAllToQueue,
		onPlayAllNext,
		onAddAllToPlaylist,
		onDownload,
		icon
	}: Props = $props();

	const hasAnyTracks = $derived(trackCount > 0);
	const hasBulkActions = $derived(Boolean(onAddAllToQueue || onPlayAllNext || onAddAllToPlaylist));

	const menuItems = $derived.by<MenuItem[]>(() => {
		const items: MenuItem[] = [];
		if (onAddAllToQueue) {
			items.push({ label: 'Add All to Queue', icon: ListPlus, onclick: onAddAllToQueue });
		}
		if (onPlayAllNext) {
			items.push({ label: 'Play All Next', icon: ListStart, onclick: onPlayAllNext });
		}
		if (onAddAllToPlaylist) {
			items.push({ label: 'Add All to Playlist', icon: ListMusic, onclick: onAddAllToPlaylist });
		}
		if (onDownload) {
			items.push({ label: 'Download Album', icon: Download, onclick: onDownload });
		}
		return items;
	});
</script>

<!-- A head row on the track panel: the source, how many tracks it has, then
     its Play All, Shuffle and menu. -->
<div class="flex flex-wrap items-center gap-x-3 gap-y-2 px-3 py-2.5 sm:px-4">
	<div class="mr-1 flex min-w-0 items-center gap-2">
		<span class="shrink-0" style:color={sourceColor}>
			{@render icon()}
		</span>
		<span class="text-sm font-bold">{sourceLabel}</span>
		{#if hasAnyTracks}
			<span
				class="inline-flex h-6 items-center rounded-full bg-base-content px-2.5 text-xs font-bold text-base-100 tabular-nums"
				>{trackCount}/{totalTracks}</span
			>
		{/if}
		{#if extraBadge}
			<span class="text-sm font-medium text-muted uppercase">{extraBadge}</span>
		{/if}
	</div>

	{#if hasAnyTracks}
		<div class="ml-auto flex items-center gap-1.5">
			<Button
				variant="primary"
				size="sm"
				icon={Play}
				class="[&_svg]:fill-current"
				onclick={onPlayAll}
			>
				Play All
			</Button>
			<Button variant="ghost" size="sm" icon={Shuffle} onclick={onShuffle}>Shuffle</Button>
			{#if hasBulkActions}
				<ContextMenu items={menuItems} position="end" size="sm" />
			{/if}
		</div>
	{/if}
</div>
