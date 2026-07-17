<script lang="ts">
	import type { WeeklyExplorationSection as SectionType } from '$lib/types';
	import { Sparkles, ExternalLink } from 'lucide-svelte';
	import HorizontalCarousel from './HorizontalCarousel.svelte';
	import WeeklyExplorationCard from './WeeklyExplorationCard.svelte';
	import LiveUpdatingBadge from './LiveUpdatingBadge.svelte';

	interface Props {
		section: SectionType;
		updating?: boolean;
	}

	let { section, updating = false }: Props = $props();

	function formatPlaylistDate(dateStr: string): string {
		try {
			const d = new Date(dateStr);
			return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric' });
		} catch {
			return '';
		}
	}

	const formattedDate = $derived(formatPlaylistDate(section.playlist_date));
</script>

<section class="my-10">
	<div class="mb-4 flex items-center gap-3">
		<div class="flex items-center gap-2">
			<Sparkles class="h-5 w-5 text-warning" />
			<h2 class="text-lg font-bold text-base-content">Weekly Exploration</h2>
		</div>

		{#if formattedDate}
			<span class="badge badge-ghost badge-sm text-base-content/50">
				{formattedDate}
			</span>
		{/if}
		{#if updating}
			<LiveUpdatingBadge label="Updating weekly picks" className="px-2 py-0.5" />
		{/if}

		{#if section.source_url}
			<a
				href={section.source_url}
				target="_blank"
				rel="noopener noreferrer"
				class="ml-auto flex items-center gap-1 text-xs text-base-content/40
					hover:text-primary transition-colors"
				title="View on ListenBrainz"
			>
				ListenBrainz
				<ExternalLink class="h-3 w-3" />
			</a>
		{/if}
	</div>

	<HorizontalCarousel>
		{#each section.tracks as track (track.artist_name + track.title)}
			<WeeklyExplorationCard {track} />
		{/each}
	</HorizontalCarousel>
</section>
