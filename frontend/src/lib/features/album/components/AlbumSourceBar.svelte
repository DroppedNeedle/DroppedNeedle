<script lang="ts">
	import type { Snippet } from 'svelte';
	import { Shuffle, Play, ListPlus, ListStart, ListMusic, Download } from 'lucide-svelte';
	import ContextMenu from '$lib/components/ContextMenu.svelte';
	import type { MenuItem } from '$lib/components/ContextMenu.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';

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

<Card variant="bordered" padding="md">
	<div class="flex items-center gap-3 flex-wrap">
		<div class="flex items-center gap-2 mr-1">
			<span style:color={sourceColor}>
				{@render icon()}
			</span>
			<span class="text-sm font-bold">{sourceLabel}</span>
			{#if hasAnyTracks}
				<span class="badge badge-sm badge-neutral">{trackCount}/{totalTracks}</span>
			{/if}
			{#if extraBadge}
				<span class="badge badge-sm badge-ghost uppercase">{extraBadge}</span>
			{/if}
		</div>

		{#if hasAnyTracks}
			<div class="flex items-center gap-2 flex-wrap ml-auto">
				<Button
					variant="accent"
					size="sm"
					icon={Play}
					class="[&_svg]:fill-current"
					onclick={onPlayAll}
				>
					Play All
				</Button>

				<Button variant="ghost" size="sm" icon={Shuffle} onclick={onShuffle}>Shuffle</Button>

				{#if hasBulkActions}
					<div class="ml-auto">
						<ContextMenu items={menuItems} position="end" size="sm" />
					</div>
				{/if}
			</div>
		{/if}
	</div>
</Card>
