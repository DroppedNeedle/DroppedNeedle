<script lang="ts">
	import { Headphones, Play, Shuffle, Sparkles, ArrowRight } from 'lucide-svelte';
	import {
		getLocalRecentV3Query,
		getLocalStatsV3Query
	} from '$lib/queries/local/LocalV3Queries.svelte';
	import { formatBytes } from '$lib/utils/formatting';
	import { buildDiscoveryQueueFromLocal } from '$lib/player/queueHelpers';
	import { playerStore } from '$lib/stores/player.svelte';
	import { playbackToast } from '$lib/stores/playbackToast.svelte';
	import { fetchLibraryTracks, libraryTrackParams } from '$lib/queries/library/libraryTracks';
	import { createLibraryTrackQueue } from '$lib/queries/library/libraryTrackQueue.svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import Button from '$lib/ui/Button.svelte';
	import { NOISE_TEXTURE } from './noiseTexture';
	import { withBasePath } from '$lib/utils/basePath';

	const statsQuery = getLocalStatsV3Query();
	const recentQuery = getLocalRecentV3Query(() => 1);
	const stats = $derived(statsQuery.data);
	const backdropCard = $derived(recentQuery.data?.[0]);

	const queue = createLibraryTrackQueue({
		buildQueue: (tracks) => buildDiscoveryQueueFromLocal(tracks),
		playQueue: (items, startIndex, shuffle) => playerStore.playQueue(items, startIndex, shuffle),
		appendQueue: (items) => playerStore.appendQueueSilent(items),
		reshuffle: () => playerStore.regenerateShuffleOrder(),
		toast: (message, type) => playbackToast.show(message, type)
	});

	let busy = $state<'' | 'play' | 'shuffle' | 'surprise'>('');

	async function start(kind: 'play' | 'shuffle') {
		if (busy) return;
		busy = kind;
		try {
			const started =
				kind === 'play' ? await queue.playAll('recent', '') : await queue.shuffleAll('');
			if (started === 'empty') playbackToast.show('No tracks to play yet', 'info');
		} finally {
			busy = '';
		}
	}

	const playAll = () => start('play');
	const shuffleAll = () => start('shuffle');

	async function surprise() {
		if (busy) return;
		busy = 'surprise';
		try {
			const page = await fetchLibraryTracks(libraryTrackParams(1, 'random'), null);
			const track = page.items[0];
			if (!track) {
				playbackToast.show('Nothing to surprise you with yet', 'info');
				return;
			}
			playerStore.playQueue(buildDiscoveryQueueFromLocal([track]), 0, false);
		} catch {
			playbackToast.show("Couldn't pick a track", 'error');
		} finally {
			busy = '';
		}
	}
</script>

<section
	class="group relative isolate overflow-hidden rounded-3xl border border-base-content/10 shadow-lg transition-all duration-300 hover:-translate-y-1 hover:border-base-content/20 hover:shadow-2xl"
>
	{#if backdropCard}
		<div
			aria-hidden="true"
			class="pointer-events-none absolute inset-0 scale-110 blur-2xl"
			data-testid="local-files-backdrop"
		>
			<AlbumImage
				albumId={backdropCard.id}
				mbid={backdropCard.release_group_mbid ?? ''}
				available={backdropCard.cover_available}
				alt=""
				size="full"
				requestSize={250}
				testId="local-files-backdrop-image"
				rounded="none"
				className="h-full w-full object-cover opacity-60"
			/>
		</div>
	{/if}

	<div
		aria-hidden="true"
		class="absolute inset-0 bg-linear-[105deg] from-base-100/96 from-28% via-base-100/72 via-62% to-localfiles/22"
	></div>
	<div
		aria-hidden="true"
		class="pointer-events-none absolute -top-16 -right-12 h-56 w-56 rounded-full bg-localfiles/28 blur-3xl"
	></div>
	<div
		aria-hidden="true"
		class="pointer-events-none absolute inset-0 opacity-[0.05]"
		style:background-image={NOISE_TEXTURE}
		style:background-size="200px"
	></div>

	<!-- stretched link under the content so empty area navigates, while playback buttons (pointer-events re-enabled) still act in place; avoids nested interactives -->
	<a
		href={withBasePath('/library/local')}
		aria-label="Enter the Listening Room"
		class="absolute inset-0 z-10 rounded-3xl focus-visible:ring-2 focus-visible:ring-localfiles focus-visible:ring-offset-2 focus-visible:ring-offset-base-100 focus-visible:outline-none"
	></a>

	<div class="pointer-events-none relative z-20 flex flex-col gap-5 p-6 sm:p-8">
		<div class="flex items-start justify-between gap-3">
			<div class="flex items-center gap-4">
				<div
					class="flex h-14 w-14 shrink-0 items-center justify-center rounded-2xl bg-localfiles/15 text-localfiles ring-1 ring-localfiles/35 transition-transform duration-300 group-hover:-rotate-6 group-hover:scale-110"
				>
					<Headphones class="h-7 w-7" />
				</div>
				<div>
					<h2 class="text-xl font-black tracking-tight sm:text-2xl">Local Files</h2>
					<p class="text-sm text-base-content/60">
						{#if stats}
							{stats.total_tracks.toLocaleString()} tracks · {stats.total_artists.toLocaleString()} artists
							· {formatBytes(stats.total_size_bytes)}
						{:else}
							Your local music collection
						{/if}
					</p>
				</div>
			</div>
			<div class="flex shrink-0 items-center gap-2 text-right text-localfiles">
				<div class="leading-tight">
					<div
						class="hidden text-[10px] font-semibold tracking-[0.2em] uppercase opacity-70 sm:block"
					>
						Enter the
					</div>
					<div class="text-sm font-black tracking-tight sm:text-base">Listening Room</div>
				</div>
				<ArrowRight class="h-5 w-5 transition-transform duration-300 group-hover:translate-x-1" />
			</div>
		</div>

		<div class="h-px w-full bg-base-content/10"></div>

		<div class="flex flex-wrap items-center gap-3">
			<Button
				size="lg"
				icon={Play}
				loading={busy === 'play'}
				class="pointer-events-auto border-0 bg-localfiles text-base-100 shadow-lg transition-transform hover:scale-[1.02]"
				onclick={playAll}>Play All</Button
			>
			<Button
				size="lg"
				icon={Shuffle}
				loading={busy === 'shuffle'}
				class="pointer-events-auto border-localfiles/40 bg-base-100/40 backdrop-blur-sm hover:bg-base-100/70"
				onclick={shuffleAll}>Shuffle</Button
			>
			<Button
				variant="ghost"
				size="lg"
				icon={Sparkles}
				loading={busy === 'surprise'}
				class="pointer-events-auto"
				onclick={surprise}>Surprise me</Button
			>
		</div>
	</div>
</section>
