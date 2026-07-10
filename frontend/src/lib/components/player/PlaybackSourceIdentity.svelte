<script lang="ts">
	/*
	 * PlaybackSourceIdentity — which service is playing right now
	 * (Jellyfin / Navidrome / Plex / Local + format, or YouTube external link).
	 */
	import { playerStore } from '$lib/stores/player.svelte';
	import JellyfinIcon from '$lib/components/JellyfinIcon.svelte';
	import NavidromeIcon from '$lib/components/NavidromeIcon.svelte';
	import PlexIcon from '$lib/components/PlexIcon.svelte';
	import { Music, ExternalLink } from 'lucide-svelte';

	function openInYouTube(): void {
		const trackSourceId = playerStore.nowPlaying?.trackSourceId;
		if (trackSourceId) {
			window.open(`https://www.youtube.com/watch?v=${trackSourceId}`, '_blank');
		}
	}

	const sourceType = $derived(playerStore.nowPlaying?.sourceType);
</script>

{#if sourceType === 'youtube'}
	<div class="tooltip tooltip-left" data-tip="Open in YouTube">
		<button
			class="btn btn-ghost btn-sm btn-circle"
			onclick={openInYouTube}
			aria-label="Open in YouTube"
		>
			<ExternalLink class="h-4 w-4" />
		</button>
	</div>
{:else if sourceType === 'jellyfin'}
	<div class="flex items-center gap-2" style="color: rgb(var(--brand-jellyfin))">
		<JellyfinIcon class="h-5 w-5" />
		<span class="hidden text-sm font-medium lg:inline">Jellyfin</span>
	</div>
{:else if sourceType === 'navidrome'}
	<div class="flex items-center gap-2" style="color: rgb(var(--brand-navidrome))">
		<NavidromeIcon class="h-5 w-5" />
		<span class="hidden text-sm font-medium lg:inline">Navidrome</span>
	</div>
{:else if sourceType === 'plex'}
	<div class="flex items-center gap-2" style="color: rgb(var(--brand-plex))">
		<PlexIcon class="h-5 w-5" />
		<span class="hidden text-sm font-medium lg:inline">Plex</span>
	</div>
{:else if sourceType === 'local'}
	<div class="flex items-center gap-2" style="color: rgb(var(--brand-localfiles))">
		<Music class="h-5 w-5" />
		<span class="hidden text-sm font-medium lg:inline"
			>Local{#if playerStore.currentQueueItem?.format}<span
					class="badge badge-xs badge-ghost ml-1 uppercase"
					>{playerStore.currentQueueItem.format}</span
				>{/if}</span
		>
	</div>
{/if}
