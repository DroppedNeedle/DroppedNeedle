<!--
	"Playing from": the playback sources that are on, the chosen one
	pressed. Shown only when there is a choice to make.
-->
<script lang="ts">
	import JellyfinIcon from '$lib/media/icons/JellyfinIcon.svelte';
	import LocalFilesIcon from '$lib/media/icons/LocalFilesIcon.svelte';
	import NavidromeIcon from '$lib/media/icons/NavidromeIcon.svelte';
	import PlexIcon from '$lib/media/icons/PlexIcon.svelte';
	import Button from '$lib/ui/Button.svelte';
	import { getSourceLabel } from '$lib/utils/sources';
	import type { PlaybackSourceType } from '$lib/features/artist/state/artistPlayback.svelte';

	interface Props {
		sources: PlaybackSourceType[];
		selected: PlaybackSourceType;
		onchoose: (source: PlaybackSourceType) => void;
	}

	let { sources, selected, onchoose }: Props = $props();

	const label = (source: PlaybackSourceType) =>
		source === 'local' ? 'Local files' : getSourceLabel(source);
</script>

{#if sources.length > 1}
	<div class="dn-play-from" role="group" aria-label="Play from">
		<span class="dn-play-from__label" aria-hidden="true">Playing from:</span>
		{#each sources as source (source)}
			<Button
				variant="ghost"
				size="xs"
				class="dn-play-from__choice"
				active={source === selected}
				aria-pressed={source === selected}
				onclick={() => onchoose(source)}
			>
				{#if source === 'local'}
					<LocalFilesIcon class="size-3.5" />
				{:else if source === 'navidrome'}
					<NavidromeIcon class="size-3.5" />
				{:else if source === 'jellyfin'}
					<JellyfinIcon class="size-3.5" />
				{:else if source === 'plex'}
					<PlexIcon class="size-3.5" />
				{/if}
				{label(source)}
			</Button>
		{/each}
	</div>
{/if}

<style>
	.dn-play-from {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.25rem 0.375rem;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		letter-spacing: 0.12em;
		text-transform: uppercase;
	}
	.dn-play-from__label {
		margin-right: 0.25rem;
		color: var(--dn-muted);
	}
	.dn-play-from :global(.dn-play-from__choice) {
		font-family: var(--font-mono);
		letter-spacing: 0.1em;
		text-transform: uppercase;
	}
	.dn-play-from :global(.dn-play-from__choice[aria-pressed='true']) {
		color: var(--dn-title);
		text-decoration: underline;
		text-decoration-thickness: 2px;
		text-underline-offset: 4px;
	}
</style>
