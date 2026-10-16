<script lang="ts">
	import { Database, Disc3, HardDrive, Users } from 'lucide-svelte';
	import JellyfinIcon from '$lib/components/JellyfinIcon.svelte';
	import NavidromeIcon from '$lib/components/NavidromeIcon.svelte';
	import type { ProfileLibraryStats } from '$lib/queries/profile/types';
	import Card from '$lib/ui/Card.svelte';

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

{#snippet stat(Icon: typeof Disc3, label: string, value: number)}
	<div class="flex flex-col items-center gap-1">
		<div class="flex items-center gap-1.5 text-base-content/50">
			<Icon class="h-3.5 w-3.5" />
			<span class="text-[10px] font-medium uppercase tracking-wider">{label}</span>
		</div>
		<span class="text-xl font-bold tabular-nums">{formatNumber(value)}</span>
	</div>
{/snippet}

<section id="libraries" class="scroll-mt-24 xl:ml-40">
	<h2
		class="mb-4 flex items-center gap-2 text-sm font-semibold uppercase tracking-widest text-base-content/50"
	>
		<Database class="h-4 w-4" />
		Your Libraries
	</h2>
	<div class="space-y-4">
		{#each libraryStats as stats (stats.source)}
			{@const SourceIcon = getSourceIcon(stats.source)}
			<Card variant="bordered" padding="none" class="overflow-hidden">
				<div>
					<div class="flex items-center gap-3 border-b border-base-300/30 px-5 py-3">
						<div
							class="flex h-8 w-8 items-center justify-center rounded-lg bg-base-300/60 {getSourceColor(
								stats.source
							)}"
						>
							<SourceIcon class="h-4 w-4" />
						</div>
						<span class="text-sm font-semibold">{stats.source}</span>
					</div>
					<div class="grid grid-cols-3 divide-x divide-base-300/30 px-1 py-4">
						{@render stat(Disc3, 'Songs', stats.total_tracks)}
						{@render stat(Database, 'Albums', stats.total_albums)}
						{@render stat(Users, 'Artists', stats.total_artists)}
					</div>
					{#if stats.total_size_human}
						<div
							class="flex items-center justify-center gap-2 border-t border-base-300/30 px-5 py-3"
						>
							<HardDrive class="h-3.5 w-3.5 text-base-content/40" />
							<span class="text-xs text-base-content/50">
								{stats.total_size_human} used
							</span>
						</div>
					{/if}
				</div>
			</Card>
		{/each}
	</div>
</section>
