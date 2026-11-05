<script lang="ts">
	import { Database, HardDrive } from 'lucide-svelte';
	import JellyfinIcon from '$lib/media/icons/JellyfinIcon.svelte';
	import NavidromeIcon from '$lib/media/icons/NavidromeIcon.svelte';
	import type { ProfileLibraryStats } from '$lib/features/profile/queries/profile.svelte';
	import StatRow from '$lib/ui/StatRow.svelte';

	interface Props {
		libraryStats: ProfileLibraryStats[];
	}

	let { libraryStats }: Props = $props();

	function getSourceIcon(source: string) {
		if (source === 'Jellyfin') return JellyfinIcon;
		if (source === 'Navidrome') return NavidromeIcon;
		if (source === 'Local Files') return HardDrive;
		return Database;
	}

	function getSourceColor(source: string): string {
		if (source === 'Jellyfin') return 'text-jellyfin';
		if (source === 'Navidrome') return 'text-navidrome';
		if (source === 'Local Files') return 'text-localfiles';
		return 'text-base-content';
	}

	function formatNumber(n: number): string {
		if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
		return n.toString();
	}
</script>

<ul class="grid gap-3 2xl:grid-cols-2">
	{#each libraryStats as stats (stats.source)}
		{@const SourceIcon = getSourceIcon(stats.source)}
		<li class="pf-box px-4 py-3.5">
			<div class="mb-3 flex items-center gap-3">
				<span class="lib-icon {getSourceColor(stats.source)}" aria-hidden="true">
					<SourceIcon class="size-5" />
				</span>
				<span class="font-semibold">{stats.source}</span>
				{#if stats.total_size_human}
					<span class="pf-meta ml-auto">{stats.total_size_human} used</span>
				{/if}
			</div>
			<StatRow
				label="{stats.source} totals"
				stats={[
					{ label: 'Songs', value: formatNumber(stats.total_tracks) },
					{ label: 'Albums', value: formatNumber(stats.total_albums) },
					{ label: 'Artists', value: formatNumber(stats.total_artists) }
				]}
			/>
		</li>
	{/each}
</ul>

<style>
	li :global(.dn-stats__value) {
		white-space: nowrap;
	}
	.lib-icon {
		display: grid;
		place-items: center;
		width: 2.25rem;
		height: 2.25rem;
		border-radius: 0.5rem;
		background-color: var(--color-base-300);
	}
</style>
