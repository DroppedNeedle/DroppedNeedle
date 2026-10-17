<script lang="ts">
	import { TOAST_DURATION } from '$lib/constants';
	import { YOUTUBE_ENDPOINTS } from '$lib/queries/youtube/endpoints';
	import { toastStore } from '$lib/stores/toast';
	import { launchTrackPlayback } from '$lib/player/launchTrackPlayback';
	import { launchYouTubePlayback } from '$lib/player/launchYouTubePlayback';
	import { compareDiscTrack, getDiscTrackKey, normalizeDiscNumber } from '$lib/player/queueHelpers';
	import YouTubeIcon from '$lib/components/YouTubeIcon.svelte';
	import { getCoverUrl } from '$lib/utils/errorHandling';
	import { api } from '$lib/api/client';
	import { Link, Download, Search, Play } from 'lucide-svelte';
	import type { Track, YouTubeLink, YouTubeTrackLink, YouTubeQuotaStatus } from '$lib/types';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';

	interface Props {
		albumId: string;
		albumName: string;
		artistName: string;
		artistId: string;
		coverUrl: string | null;
		tracks: Track[];
		trackLinks: YouTubeTrackLink[];
		albumLink: YouTubeLink | null;
		apiConfigured: boolean;
		onTrackLinksUpdate: (links: YouTubeTrackLink[]) => void;
		onAlbumLinkUpdate: (link: YouTubeLink) => void;
		onQuotaUpdate: (quota: YouTubeQuotaStatus) => void;
	}

	let {
		albumId,
		albumName,
		artistName,
		artistId,
		coverUrl,
		tracks,
		trackLinks,
		albumLink,
		apiConfigured,
		onTrackLinksUpdate,
		onAlbumLinkUpdate,
		onQuotaUpdate
	}: Props = $props();

	let batchGenerating = $state(false);
	let generatingAlbumLink = $state(false);

	const hasAnyTrackLinks = $derived(trackLinks.length > 0);
	const allTracksGenerated = $derived(trackLinks.length === tracks.length);
	const generatedCount = $derived(trackLinks.length);

	const youtubeSearchUrl = $derived(
		`https://www.youtube.com/results?search_query=${encodeURIComponent(`${artistName} ${albumName} full album`)}`
	);

	function playAll(): void {
		if (trackLinks.length === 0) return;
		launchTrackPlayback([...trackLinks].sort(compareDiscTrack), 0, false, {
			albumId,
			albumName,
			artistName,
			coverUrl,
			artistId
		});
	}

	async function generateAlbumLink(): Promise<void> {
		if (albumLink?.video_id) return;
		generatingAlbumLink = true;
		try {
			const data = await api.global.v3.POST(YOUTUBE_ENDPOINTS.generate(), {
				artist_name: artistName,
				album_name: albumName,
				album_id: albumId,
				cover_url: getCoverUrl(coverUrl, albumId)
			});
			onAlbumLinkUpdate(data.link);
			onQuotaUpdate(data.quota);
			toastStore.show({
				message: 'Album link generated',
				type: 'success',
				duration: TOAST_DURATION
			});
		} catch (e) {
			toastStore.show({
				message: e instanceof Error ? e.message : "Couldn't build the album link",
				type: 'error',
				duration: TOAST_DURATION
			});
		} finally {
			generatingAlbumLink = false;
		}
	}

	async function generateAllTracks(): Promise<void> {
		const ungeneratedTracks = tracks.filter(
			(t) => !trackLinks.some((tl) => getDiscTrackKey(tl) === getDiscTrackKey(t))
		);
		if (ungeneratedTracks.length === 0) return;

		const cost = ungeneratedTracks.length;
		if (!confirm(`This will use ${cost} YouTube API quota unit${cost > 1 ? 's' : ''}. Continue?`))
			return;

		batchGenerating = true;
		try {
			const data = await api.global.v3.POST(YOUTUBE_ENDPOINTS.generateTracks(), {
				album_id: albumId,
				album_name: albumName,
				artist_name: artistName,
				cover_url: getCoverUrl(coverUrl, albumId),
				tracks: ungeneratedTracks.map((t) => ({
					track_name: t.title,
					track_number: t.position,
					disc_number: normalizeDiscNumber(t.disc_number)
				}))
			});
			const existing = trackLinks.filter(
				(tl) => !data.track_links.some((nl) => getDiscTrackKey(nl) === getDiscTrackKey(tl))
			);
			onTrackLinksUpdate([...existing, ...data.track_links].sort(compareDiscTrack));
			onQuotaUpdate(data.quota);

			if (data.failed.length > 0) {
				toastStore.show({
					message: `${data.failed.length} track${data.failed.length > 1 ? 's' : ''} failed`,
					type: 'error',
					duration: TOAST_DURATION
				});
			} else {
				toastStore.show({
					message: 'All tracks generated',
					type: 'success',
					duration: TOAST_DURATION
				});
			}
		} catch (e) {
			toastStore.show({
				message: e instanceof Error ? e.message : 'Batch generation failed',
				type: 'error',
				duration: TOAST_DURATION
			});
		} finally {
			batchGenerating = false;
		}
	}

	async function playAlbumLink(): Promise<void> {
		if (!albumLink?.video_id) return;
		try {
			await launchYouTubePlayback(
				{
					albumId: albumLink.album_id,
					albumName: albumLink.album_name,
					artistName: albumLink.artist_name,
					coverUrl: getCoverUrl(albumLink.cover_url || coverUrl, albumId),
					videoId: albumLink.video_id,
					embedUrl: albumLink.embed_url ?? undefined,
					artistId
				},
				{
					onLoadError: () => {
						toastStore.show({
							message: "Couldn't load the video",
							type: 'error',
							duration: TOAST_DURATION
						});
					}
				}
			);
		} catch (_e) {
			toastStore.show({
				message: "Couldn't play the album video",
				type: 'error',
				duration: TOAST_DURATION
			});
		}
	}
</script>

<Card variant="bordered" padding="md">
	<div class="flex items-center gap-3 flex-wrap">
		<div class="flex items-center gap-2 mr-1">
			<YouTubeIcon class="h-5 w-5 text-red-500" />
			<span class="text-sm font-bold">YouTube</span>
			{#if hasAnyTrackLinks}
				<span class="badge badge-sm badge-neutral">{generatedCount}/{tracks.length}</span>
			{/if}
		</div>

		<div class="flex gap-2 flex-wrap">
			{#if hasAnyTrackLinks}
				<Button
					variant="accent"
					size="sm"
					icon={Play}
					class="[&_svg]:fill-current"
					onclick={playAll}
				>
					Play All
				</Button>
			{/if}

			{#if albumLink?.video_id}
				<Button
					variant="ghost"
					size="sm"
					icon={Play}
					class="[&_svg]:fill-current"
					onclick={() => void playAlbumLink()}
				>
					Full Album
				</Button>
			{:else if apiConfigured}
				<Button size="sm" icon={Link} loading={generatingAlbumLink} onclick={generateAlbumLink}>
					Album Link
				</Button>
			{:else}
				<div
					class="tooltip tooltip-bottom"
					data-tip="Enable YouTube API in settings for auto-generation"
				>
					<Button size="sm" icon={Link} disabled>Album Link</Button>
				</div>
			{/if}

			{#if !allTracksGenerated}
				{#if apiConfigured}
					<Button size="sm" icon={Download} loading={batchGenerating} onclick={generateAllTracks}>
						All Tracks
					</Button>
				{:else}
					<div
						class="tooltip tooltip-bottom"
						data-tip="Enable YouTube API in settings for auto-generation"
					>
						<Button size="sm" icon={Download} disabled>All Tracks</Button>
					</div>
				{/if}
			{/if}

			<Button
				variant="ghost"
				size="sm"
				icon={Search}
				href={youtubeSearchUrl}
				target="_blank"
				rel="noopener noreferrer"
			>
				Search
			</Button>
		</div>
	</div>
</Card>
