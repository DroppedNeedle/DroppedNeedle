<script lang="ts">
	import { Download, Play } from 'lucide-svelte';
	import { API, TOAST_DURATION } from '$lib/constants';
	import { toastStore } from '$lib/stores/toast';
	import { launchTrackPlayback } from '$lib/player/launchTrackPlayback';
	import { compareDiscTrack, getDiscTrackKey, normalizeDiscNumber } from '$lib/player/queueHelpers';
	import YouTubeIcon from '$lib/components/YouTubeIcon.svelte';
	import { getCoverUrl } from '$lib/utils/errorHandling';
	import { api } from '$lib/api/client';
	import type { YouTubeTrackLink, YouTubeTrackLinkResponse, YouTubeQuotaStatus } from '$lib/types';

	interface Props {
		trackNumber: number;
		discNumber?: number;
		trackName: string;
		trackLink: YouTubeTrackLink | null;
		allTrackLinks: YouTubeTrackLink[];
		albumId: string;
		albumName: string;
		artistName: string;
		coverUrl: string | null;
		artistId?: string;
		apiConfigured: boolean;
		onGenerated: (link: YouTubeTrackLink) => void;
		onQuotaUpdate: (quota: YouTubeQuotaStatus) => void;
	}

	let {
		trackNumber,
		discNumber = 1,
		trackName,
		trackLink,
		allTrackLinks,
		albumId,
		albumName,
		artistName,
		coverUrl,
		artistId,
		apiConfigured,
		onGenerated,
		onQuotaUpdate
	}: Props = $props();

	let generating = $state(false);

	const hasLink = $derived(trackLink !== null);
	const playerCoverUrl = $derived(getCoverUrl(coverUrl, albumId));

	function play(): void {
		if (!trackLink) return;
		const sortedTrackLinks = [...allTrackLinks].sort(compareDiscTrack);
		const currentKey = getDiscTrackKey({ disc_number: discNumber, track_number: trackNumber });
		const idx = sortedTrackLinks.findIndex((tl) => getDiscTrackKey(tl) === currentKey);
		if (idx === -1) return;
		launchTrackPlayback(sortedTrackLinks, idx, false, {
			albumId,
			albumName,
			artistName,
			coverUrl: playerCoverUrl,
			artistId
		});
	}

	async function generateLink(): Promise<void> {
		generating = true;
		try {
			const data = await api.global.post<YouTubeTrackLinkResponse>(API.youtube.generateTrack(), {
				album_id: albumId,
				album_name: albumName,
				artist_name: artistName,
				track_name: trackName,
				track_number: trackNumber,
				disc_number: normalizeDiscNumber(discNumber),
				cover_url: getCoverUrl(coverUrl, albumId)
			});
			onGenerated(data.track_link);
			onQuotaUpdate(data.quota);
		} catch (e) {
			toastStore.show({
				message: `Failed: ${trackName} (${e instanceof Error ? e.message : 'Unknown error'})`,
				type: 'error',
				duration: TOAST_DURATION
			});
		} finally {
			generating = false;
		}
	}

	function handleClick(): void {
		if (hasLink) {
			play();
		} else if (apiConfigured) {
			void generateLink();
		} else {
			toastStore.show({
				message: 'YouTube API not configured. Enable it in Settings.',
				type: 'error',
				duration: TOAST_DURATION
			});
		}
	}
</script>

{#if !hasLink && !apiConfigured}{:else if generating}
	<button
		class="btn btn-sm btn-ghost rounded-lg gap-1.5 shrink-0"
		disabled
		aria-label="Generating link"
	>
		<span class="loading loading-spinner loading-xs"></span>
		<YouTubeIcon class="h-4 w-4" />
	</button>
{:else}
	<button
		class="btn btn-sm rounded-lg gap-1.5 shrink-0"
		class:btn-error={hasLink}
		class:btn-ghost={!hasLink}
		onclick={handleClick}
		aria-label={hasLink ? 'Play on YouTube' : 'Generate YouTube link'}
	>
		{#if hasLink}
			<Play class="h-4 w-4 fill-current" />
		{:else}
			<Download class="h-4 w-4" />
		{/if}
		<YouTubeIcon class="h-4 w-4" />
	</button>
{/if}
