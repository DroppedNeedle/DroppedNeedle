<script lang="ts">
	import { ChevronDown, Check } from 'lucide-svelte';
	import { getSourceLabel, getSourceColor } from '$lib/utils/sources';
	import JellyfinIcon from '$lib/media/icons/JellyfinIcon.svelte';
	import LocalFilesIcon from '$lib/media/icons/LocalFilesIcon.svelte';
	import YouTubeIcon from '$lib/media/icons/YouTubeIcon.svelte';
	import NavidromeIcon from '$lib/media/icons/NavidromeIcon.svelte';
	import PlexIcon from '$lib/media/icons/PlexIcon.svelte';

	interface Props {
		currentSource: string;
		availableSources: string[];
		onchange: (source: string) => void;
	}

	let { currentSource, availableSources, onchange }: Props = $props();

	let hasMultiple = $derived(availableSources.length > 1);
	let isOpen = $state(false);
	let containerEl = $state<HTMLDivElement | null>(null);

	function toggleOpen(e: MouseEvent) {
		e.stopPropagation();
		isOpen = !isOpen;
	}

	function handleItemClick(source: string, e: MouseEvent) {
		e.stopPropagation();
		isOpen = false;
		onchange(source);
	}

	function handleClickOutside(e: MouseEvent) {
		if (isOpen && containerEl && !containerEl.contains(e.target as Node)) {
			isOpen = false;
		}
	}
</script>

<svelte:window onclick={handleClickOutside} />

{#snippet sourceIcon(source: string, cls: string)}
	{#if source === 'jellyfin'}
		<JellyfinIcon class={cls} />
	{:else if source === 'navidrome'}
		<NavidromeIcon class={cls} />
	{:else if source === 'local'}
		<LocalFilesIcon class={cls} />
	{:else if source === 'youtube'}
		<YouTubeIcon class={cls} />
	{:else if source === 'plex'}
		<PlexIcon class={cls} />
	{/if}
{/snippet}

{#if hasMultiple}
	<div bind:this={containerEl} class="dropdown dropdown-end shrink-0" class:dropdown-open={isOpen}>
		<button
			type="button"
			class="badge badge-sm gap-1.5 cursor-pointer select-none border-base-content/15 hover:border-base-content/30 transition-colors"
			style="color: {getSourceColor(currentSource)};"
			onclick={toggleOpen}
		>
			{@render sourceIcon(currentSource, 'h-3 w-3')}
			<span class="text-xs font-medium">{getSourceLabel(currentSource)}</span>
			<ChevronDown class="h-3 w-3 opacity-50" />
		</button>
		{#if isOpen}
			<ul
				class="dropdown-content menu bg-base-200/95 backdrop-blur-md rounded-box shadow-xl border border-base-300 p-1 min-w-36 z-50 mt-1"
			>
				{#each availableSources as source (source)}
					<li>
						<button
							type="button"
							class="flex items-center gap-2 text-sm"
							style="color: {getSourceColor(source)};"
							onclick={(e) => handleItemClick(source, e)}
						>
							{@render sourceIcon(source, 'h-4 w-4')}
							<span class="flex-1">{getSourceLabel(source)}</span>
							{#if source === currentSource}
								<Check class="h-3.5 w-3.5" />
							{/if}
						</button>
					</li>
				{/each}
			</ul>
		{/if}
	</div>
{:else}
	<span
		class="badge badge-sm gap-1.5 border-base-content/10 shrink-0"
		style="color: {getSourceColor(currentSource)};"
	>
		{@render sourceIcon(currentSource, 'h-3 w-3')}
		<span class="text-xs font-medium">{getSourceLabel(currentSource)}</span>
	</span>
{/if}
