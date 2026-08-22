<script lang="ts">
	import { resolve } from '$app/paths';
	import { Headphones, Play, Shuffle, Sparkles, ArrowRight } from 'lucide-svelte';
	import { getLocalStatsQuery, getLocalRecentQuery } from '$lib/queries/local/LocalQueries.svelte';
	import { createLibraryTrackLoader } from '$lib/utils/libraryTrackLoader.svelte';
	import { buildDiscoveryQueueFromLocal } from '$lib/player/queueHelpers';
	import { playerStore } from '$lib/stores/player.svelte';
	import { playbackToast } from '$lib/stores/playbackToast.svelte';
	import { API } from '$lib/constants';
	import { api } from '$lib/api/client';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import type { NativeTrackListItem, NativeTrackPage } from '$lib/types';

	const statsQuery = getLocalStatsQuery();
	const recentQuery = getLocalRecentQuery();
	const stats = $derived(statsQuery.data);
	const backdropAlbum = $derived(recentQuery.data?.[0] ?? null);

	const PAGE_SIZE = 100;
	const loader = createLibraryTrackLoader<NativeTrackListItem>(
		{
			fetchPageUrl: (limit, offset) => API.library.tracks(limit, offset, 'recent'),
			buildQueue: (tracks) => buildDiscoveryQueueFromLocal(tracks),
			pageSize: PAGE_SIZE
		},
		(items) => playerStore.appendQueueSilent(items),
		(items, startIndex, shuffle) => playerStore.playQueue(items, startIndex, shuffle),
		() => playerStore.regenerateShuffleOrder(),
		(message, type) => playbackToast.show(message, type)
	);

	let busy = $state<'' | 'play' | 'shuffle' | 'surprise'>('');

	function firstPage(): Promise<NativeTrackPage> {
		return api.global.get<NativeTrackPage>(API.library.tracks(PAGE_SIZE, 0, 'recent'));
	}

	async function playAll() {
		if (busy) return;
		busy = 'play';
		try {
			const page = await firstPage();
			if (!page.items.length) {
				playbackToast.show('No tracks to play yet', 'info');
				return;
			}
			loader.playAll(page.items, page.total);
		} catch {
			playbackToast.show("Couldn't start playback", 'error');
		} finally {
			busy = '';
		}
	}

	async function shuffleAll() {
		if (busy) return;
		busy = 'shuffle';
		try {
			const page = await firstPage();
			if (!page.items.length) {
				playbackToast.show('No tracks to play yet', 'info');
				return;
			}
			loader.shuffleAll(page.items, page.total);
		} catch {
			playbackToast.show("Couldn't start playback", 'error');
		} finally {
			busy = '';
		}
	}

	async function surprise() {
		if (busy) return;
		busy = 'surprise';
		try {
			const count = stats?.total_tracks ?? 0;
			const offset = count > 0 ? Math.floor(Math.random() * count) : 0;
			const page = await api.global.get<NativeTrackPage>(API.library.tracks(1, offset, 'recent'));
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
	{#if backdropAlbum}
		<div
			aria-hidden="true"
			class="pointer-events-none absolute inset-0 scale-110 blur-2xl"
			data-testid="local-files-backdrop"
		>
			<AlbumImage
				mbid={backdropAlbum.musicbrainz_id}
				customUrl={backdropAlbum.cover_url}
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
		class="absolute inset-0"
		style="background:
			linear-gradient(105deg, oklch(from var(--color-base-100) l c h / 0.96) 28%, oklch(from var(--color-base-100) l c h / 0.72) 62%, rgb(var(--brand-localfiles) / 0.22) 100%);"
	></div>
	<div
		aria-hidden="true"
		class="pointer-events-none absolute -top-16 -right-12 h-56 w-56 rounded-full blur-3xl"
		style="background: rgb(var(--brand-localfiles) / 0.28);"
	></div>
	<div
		aria-hidden="true"
		class="pointer-events-none absolute inset-0 opacity-[0.05]"
		style="background-image: url('data:image/svg+xml,<svg xmlns=%22http://www.w3.org/2000/svg%22 viewBox=%220 0 200 200%22><filter id=%22n%22><feTurbulence type=%22fractalNoise%22 baseFrequency=%220.9%22 numOctaves=%224%22 stitchTiles=%22stitch%22/></filter><rect width=%22100%25%22 height=%22100%25%22 filter=%22url(%23n)%22 opacity=%220.5%22/></svg>'); background-size: 200px;"
	></div>

	<!-- stretched link under the content so empty area navigates, while playback buttons (pointer-events re-enabled) still act in place; avoids nested interactives -->
	<a
		href={resolve('/library/local')}
		aria-label="Enter the Listening Room"
		class="absolute inset-0 z-10 rounded-3xl focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:ring-offset-base-100 focus-visible:outline-none"
		style="--tw-ring-color: rgb(var(--brand-localfiles));"
	></a>

	<div class="pointer-events-none relative z-20 flex flex-col gap-5 p-6 sm:p-8">
		<div class="flex items-start justify-between gap-3">
			<div class="flex items-center gap-4">
				<div
					class="flex h-14 w-14 shrink-0 items-center justify-center rounded-2xl ring-1 transition-transform duration-300 group-hover:-rotate-6 group-hover:scale-110"
					style="background: rgb(var(--brand-localfiles) / 0.15); color: rgb(var(--brand-localfiles)); --tw-ring-color: rgb(var(--brand-localfiles) / 0.35);"
				>
					<Headphones class="h-7 w-7" />
				</div>
				<div>
					<h2 class="text-xl font-black tracking-tight sm:text-2xl">Local Files</h2>
					<p class="text-sm text-base-content/60">
						{#if stats}
							{stats.total_tracks.toLocaleString()} tracks · {stats.total_artists.toLocaleString()} artists
							· {stats.total_size_human}
						{:else}
							Your local music collection
						{/if}
					</p>
				</div>
			</div>
			<div
				class="flex shrink-0 items-center gap-2 text-right"
				style="color: rgb(var(--brand-localfiles));"
			>
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
			<button
				onclick={playAll}
				disabled={busy === 'play'}
				class="btn btn-lg pointer-events-auto gap-2 border-0 text-base-100 shadow-lg transition-transform hover:scale-[1.02]"
				style="background: rgb(var(--brand-localfiles));"
			>
				{#if busy === 'play'}
					<span class="loading loading-spinner loading-sm"></span>
				{:else}
					<Play class="h-5 w-5" />
				{/if}
				Play All
			</button>
			<button
				onclick={shuffleAll}
				disabled={busy === 'shuffle'}
				class="btn btn-lg pointer-events-auto gap-2 border bg-base-100/40 backdrop-blur-sm hover:bg-base-100/70"
				style="border-color: rgb(var(--brand-localfiles) / 0.4);"
			>
				{#if busy === 'shuffle'}
					<span class="loading loading-spinner loading-sm"></span>
				{:else}
					<Shuffle class="h-5 w-5" />
				{/if}
				Shuffle
			</button>
			<button
				onclick={surprise}
				disabled={busy === 'surprise'}
				class="btn btn-lg btn-ghost pointer-events-auto gap-2"
			>
				{#if busy === 'surprise'}
					<span class="loading loading-spinner loading-sm"></span>
				{:else}
					<Sparkles class="h-5 w-5" />
				{/if}
				Surprise me
			</button>
		</div>
	</div>
</section>
