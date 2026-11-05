<!--
	A discovery shelf: the Anton heading with its mono line, Play all,
	Shuffle and Refresh at the right, the genre pills, then the tracks in a
	panel (or the line saying why there are none).
-->
<script lang="ts">
	import './hub.css';
	import { Play, RefreshCw, Shuffle } from 'lucide-svelte';
	import type { Snippet } from 'svelte';
	import Button from '$lib/ui/Button.svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';
	import type { KitIcon } from '$lib/ui/Button.svelte';

	interface Props {
		title: string;
		icon: KitIcon;
		/** A mono line beside the title. */
		sub?: string;
		loading?: boolean;
		empty?: boolean;
		emptyMessage?: string;
		onrefresh?: () => void;
		/** Shown while there are tracks to play. */
		onplayall?: () => void;
		playAllLabel?: string;
		onshuffle?: () => void;
		/** The genre pills, above the panel. */
		filters?: Snippet;
		/** No panel round the content (rows of covers). */
		bare?: boolean;
		children: Snippet;
	}

	let {
		title,
		icon,
		sub,
		loading = false,
		empty = false,
		emptyMessage = 'Nothing here yet.',
		onrefresh,
		onplayall,
		playAllLabel = 'Play all',
		onshuffle,
		filters,
		bare = false,
		children
	}: Props = $props();
</script>

<section class="hub-disc">
	<SectionHead {title} {icon}>
		{#snippet chips()}
			{#if sub}<p class="hub-sub">{sub}</p>{/if}
		{/snippet}
		{#snippet actions()}
			<div class="hub-disc__actions">
				{#if onplayall && !loading && !empty}
					<Button variant="primary" size="display-sm" icon={Play} onclick={onplayall}>
						{playAllLabel}
					</Button>
				{/if}
				{#if onshuffle && !loading && !empty}
					<Button variant="outline" size="display-sm" icon={Shuffle} onclick={onshuffle}>
						Shuffle
					</Button>
				{/if}
				{#if onrefresh}
					<Button
						variant="outline"
						size="display-sm"
						icon={RefreshCw}
						{loading}
						onclick={onrefresh}
					>
						Refresh
					</Button>
				{/if}
			</div>
		{/snippet}
	</SectionHead>

	{#if filters}
		<div class="hub-disc__filters">{@render filters()}</div>
	{/if}

	<div
		class={bare
			? 'hub-disc__body'
			: `hub-panel hub-disc__body hub-disc__body--panel ${empty && !loading ? 'hub-disc__body--empty' : ''}`}
	>
		{#if loading}
			<div class="space-y-2 p-2" role="status" aria-label="Loading {title}">
				{#each Array(5) as _, i (i)}
					<div class="skeleton skeleton-shimmer h-7 w-full rounded"></div>
				{/each}
			</div>
		{:else if empty}
			<p class="hub-disc__empty">{emptyMessage}</p>
		{:else}
			{@render children()}
		{/if}
	</div>
</section>

<style>
	.hub-disc {
		display: flex;
		min-width: 0;
		flex-direction: column;
	}
	.hub-disc :global(.dn-section-head) {
		flex-wrap: wrap;
		row-gap: 0.625rem;
	}
	.hub-disc__actions {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
	}
	.hub-disc__filters {
		min-width: 0;
		margin-bottom: 0.75rem;
	}
	.hub-disc__body {
		flex: 1;
	}
	.hub-disc__body--panel {
		padding: 0.25rem 0.5rem;
	}
	.hub-disc__body--empty {
		flex: none;
		border-style: dashed;
		background: transparent;
	}
	.hub-disc__empty {
		padding: 0.875rem 0.75rem;
		text-align: center;
		font-size: 0.875rem;
		color: var(--dn-muted);
	}

	@media (max-width: 639px) {
		.hub-disc__actions {
			display: grid;
			flex: 1 1 100%;
			grid-auto-columns: minmax(0, 1fr);
			grid-auto-flow: column;
		}
		.hub-disc__actions :global(.btn) {
			--btn-p: 0.375rem;
			gap: 0.375rem;
		}
	}
</style>
