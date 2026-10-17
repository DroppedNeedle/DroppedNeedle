<!-- A destination's live badge, drawn in the style of the nav it sits in. -->
<script lang="ts">
	import { CircleArrowUp } from 'lucide-svelte';
	import { syncStatus } from '$lib/stores/syncStatus.svelte';
	import ConcertsNavBadge from '$lib/components/ConcertsNavBadge.svelte';
	import DownloadsNavBadge from '$lib/components/DownloadsNavBadge.svelte';
	import NewReleasesNavBadge from '$lib/components/NewReleasesNavBadge.svelte';
	import PendingApprovalNavBadge from '$lib/components/PendingApprovalNavBadge.svelte';
	import type { NavBadge } from './navItems';

	interface Props {
		badge: NavBadge;
		place: 'sidebar' | 'bottom' | 'more';
		updateAvailable: boolean;
	}

	let { badge, place, updateAvailable }: Props = $props();
</script>

{#if badge === 'library-sync'}
	{#if syncStatus.isActive}
		{#if place === 'bottom'}
			<span class="droppedneedle-bottom-nav__badge" aria-label="Library sync in progress"></span>
		{:else}
			<span
				class="absolute -top-1 -right-1 badge badge-primary badge-xs w-2.5 h-2.5 p-0 animate-pulse"
				aria-label="Library sync in progress"
			></span>
		{/if}
	{/if}
{:else if badge === 'downloads'}
	<DownloadsNavBadge />
{:else if badge === 'following'}
	<!-- an overlapping pair: new releases left, concerts right -->
	<ConcertsNavBadge />
	<NewReleasesNavBadge />
{:else if badge === 'approvals'}
	<PendingApprovalNavBadge />
{:else if badge === 'update' && updateAvailable}
	{#if place === 'bottom'}
		<span class="droppedneedle-bottom-nav__badge" aria-label="Update available">
			<CircleArrowUp class="h-3 w-3" />
		</span>
	{:else}
		<span
			class="pointer-events-none absolute -top-0.5 -right-0.5 flex h-4.5 w-4.5 items-center justify-center rounded-full bg-accent text-accent-content shadow-sm shadow-accent/30"
		>
			<CircleArrowUp class="h-3 w-3" />
		</span>
	{/if}
{/if}

<style>
	.droppedneedle-bottom-nav__badge {
		position: absolute;
		top: 0.35rem;
		right: max(0.6rem, 20%);
		min-width: 1rem;
		height: 1rem;
		padding: 0 0.25rem;
		border-radius: 999px;
		background: oklch(from var(--color-info) l c h);
		color: oklch(from var(--color-info-content) l c h);
		font-size: 0.625rem;
		font-weight: 800;
		line-height: 1rem;
	}
</style>
