<script lang="ts">
	import { Check, Download, Headphones, Loader2, Play, Volume2 } from 'lucide-svelte';
	import { SvelteSet } from 'svelte/reactivity';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import { deckSampler, type SampleEntry } from '$lib/stores/deckSampler.svelte';
	import { integrationStore } from '$lib/stores/integration';
	import { libraryStore } from '$lib/stores/library';
	import { requestAlbum } from '$lib/utils/albumRequest';
	import { albumHrefOrNull } from '$lib/utils/entityRoutes';
	import type { HomeAlbum, HomeSection, TopPicksSection } from '$lib/types';
	import LiveUpdatingBadge from '$lib/components/LiveUpdatingBadge.svelte';

	interface Props {
		section: HomeSection | null;
		topPicks: TopPicksSection | null;
		updating?: boolean;
	}

	let { section, topPicks, updating = false }: Props = $props();

	let requesting = $state(false);

	// browse-by-ear pool: listeners-like-you albums + top-pick runners-up
	const pool = $derived.by(() => {
		const seen = new SvelteSet<string>();
		const albums: HomeAlbum[] = [];
		for (const item of (section?.items ?? []) as HomeAlbum[]) {
			if (item.mbid && !seen.has(item.mbid)) {
				seen.add(item.mbid);
				albums.push(item);
			}
		}
		for (const pick of topPicks?.items.slice(3) ?? []) {
			if (pick.album.mbid && !seen.has(pick.album.mbid)) {
				seen.add(pick.album.mbid);
				albums.push(pick.album);
			}
		}
		return albums.slice(0, 15);
	});

	const activeAlbum = $derived(
		pool.find((a) => a.mbid === deckSampler.activeKey && deckSampler.status !== 'idle') ?? null
	);
	const activeRequested = $derived(
		!!activeAlbum && (activeAlbum.requested || libraryStore.isRequested(activeAlbum.mbid))
	);

	function albumEntry(album: HomeAlbum): SampleEntry {
		// pool guarantees a non-null mbid (filtered below), so the coalesce never fires
		return {
			key: album.mbid ?? '',
			kind: 'album',
			artist: album.artist_name ?? '',
			title: album.name,
			albumMbid: album.mbid,
			artistMbid: album.artist_mbid,
			coverUrl: album.image_url
		};
	}

	function sample(album: HomeAlbum) {
		if (!album.mbid) return;
		if (deckSampler.activeKey === album.mbid && deckSampler.status !== 'idle') {
			deckSampler.stop();
			return;
		}
		void deckSampler.start(album.mbid, album.artist_name ?? '', album.name, {
			artistMbid: album.artist_mbid,
			coverUrl: album.image_url
		});
	}

	function playAll() {
		if (pool.length === 0) return;
		// lean-back: hear the whole shelf as a 30s-preview station in the widget
		deckSampler.startStation('Listening Lounge', pool.map(albumEntry));
	}

	async function requestActive() {
		if (!activeAlbum?.mbid || requesting) return;
		requesting = true;
		try {
			await requestAlbum(activeAlbum.mbid, {
				artist: activeAlbum.artist_name ?? undefined,
				album: activeAlbum.name,
				artistMbid: activeAlbum.artist_mbid ?? undefined
			});
		} finally {
			requesting = false;
		}
	}

	const RING_R = 15;
	const RING_C = 2 * Math.PI * RING_R;
</script>

{#if pool.length > 0}
	<section
		class="relative overflow-hidden rounded-2xl border border-secondary/15 bg-gradient-to-br from-secondary/8 via-base-200/50 to-primary/6 p-5 shadow-[0_4px_24px_oklch(from_var(--color-secondary)_l_c_h_/_0.08)] sm:p-6"
	>
		<div class="mb-4 flex flex-wrap items-center gap-x-4 gap-y-2">
			<div class="flex items-center gap-2">
				<span class="animate-glow-pulse rounded-lg p-1">
					<Headphones class="h-5 w-5 text-secondary" />
				</span>
				<div>
					<h2 class="text-lg font-bold sm:text-xl">Listening Lounge</h2>
					<p class="text-xs text-base-content/50">
						Tap a cover to hear it - albums picked for your ears
					</p>
				</div>
			</div>

			<div class="ml-auto flex items-center gap-3">
				{#if updating}
					<LiveUpdatingBadge label="Updating lounge" className="px-2 py-0.5" />
				{/if}
				<button class="btn btn-secondary btn-sm gap-2" onclick={playAll}>
					<Play class="h-4 w-4" fill="currentColor" />
					Play all
				</button>
				<label class="flex items-center gap-1.5" title="Preview volume">
					<Volume2 class="h-3.5 w-3.5 shrink-0 text-base-content/40" />
					<input
						type="range"
						min="0"
						max="1"
						step="0.05"
						value={deckSampler.volume}
						oninput={(e) => deckSampler.setVolume(Number(e.currentTarget.value))}
						class="range range-secondary range-xs w-20"
						aria-label="Preview volume"
					/>
				</label>
			</div>
		</div>

		<div class="flex gap-4 overflow-x-auto pb-2">
			{#each pool as album (album.mbid)}
				{@const isActive = album.mbid === deckSampler.activeKey && deckSampler.status !== 'idle'}
				{@const href = albumHrefOrNull(album.mbid)}
				<div class="w-40 shrink-0 sm:w-48">
					<button
						class="lounge-cover group/card relative w-full overflow-hidden rounded-xl shadow-md transition-all duration-300 {isActive
							? 'ring-2 ring-secondary ring-offset-2 ring-offset-base-100'
							: 'hover:-translate-y-1 hover:shadow-xl'}"
						onclick={() => sample(album)}
						aria-label="{isActive ? 'Stop' : 'Preview'} {album.name} by {album.artist_name}"
						title={isActive ? 'Stop preview' : 'Preview this album'}
					>
						<AlbumImage
							mbid={album.mbid || ''}
							alt={album.name}
							size="full"
							lazy={true}
							rounded="none"
							className="block aspect-square w-full object-cover"
							customUrl={album.image_url || null}
						/>
						<div
							class="absolute inset-0 flex items-center justify-center bg-black/0 transition-colors duration-300 {isActive
								? 'bg-black/35'
								: 'group-hover/card:bg-black/25'}"
						>
							{#if isActive && deckSampler.status === 'loading'}
								<Loader2 class="h-8 w-8 animate-spin text-white drop-shadow" />
							{:else if isActive}
								<svg viewBox="0 0 40 40" class="h-12 w-12 -rotate-90 drop-shadow">
									<circle
										cx="20"
										cy="20"
										r={RING_R}
										fill="none"
										stroke="white"
										stroke-opacity="0.3"
										stroke-width="3"
									/>
									<circle
										cx="20"
										cy="20"
										r={RING_R}
										fill="none"
										stroke="white"
										stroke-width="3"
										stroke-linecap="round"
										stroke-dasharray={RING_C}
										stroke-dashoffset={RING_C * (1 - deckSampler.progress)}
									/>
								</svg>
							{:else}
								<span
									class="flex h-11 w-11 items-center justify-center rounded-full bg-white/85 opacity-0 shadow-lg transition-opacity duration-200 group-hover/card:opacity-100"
								>
									<Play class="h-5 w-5 text-black" fill="currentColor" />
								</span>
							{/if}
						</div>
					</button>
					<svelte:element
						this={href ? 'a' : 'span'}
						href={href ?? undefined}
						class="mt-1.5 block truncate text-xs font-semibold {href
							? 'transition-colors hover:text-secondary'
							: ''}"
					>
						{album.name}
					</svelte:element>
					<p class="truncate text-xs text-base-content/50">{album.artist_name}</p>
				</div>
			{/each}
		</div>

		{#if activeAlbum}
			<div
				class="mt-3 flex items-center gap-3 rounded-xl border border-secondary/15 bg-base-100/60 px-4 py-2 backdrop-blur-sm"
			>
				<span class="text-secondary">♪</span>
				<p class="min-w-0 flex-1 truncate text-sm">
					<span class="font-semibold">{activeAlbum.artist_name}</span>
					{#if deckSampler.currentTrack}
						<span class="text-base-content/60">- {deckSampler.currentTrack.title}</span>
					{/if}
					{#if deckSampler.provider}
						<span class="ml-2 text-[0.65rem] uppercase tracking-wide text-base-content/35">
							via {deckSampler.provider === 'deezer' ? 'Deezer' : 'iTunes'}
						</span>
					{/if}
				</p>
				{#if $integrationStore.download_client && !activeAlbum.in_library}
					<button
						class="btn btn-secondary btn-xs gap-1.5"
						onclick={requestActive}
						disabled={requesting || activeRequested}
					>
						{#if requesting}
							<Loader2 class="h-3.5 w-3.5 animate-spin" />
						{:else if activeRequested}
							<Check class="h-3.5 w-3.5" />
						{:else}
							<Download class="h-3.5 w-3.5" />
						{/if}
						{activeRequested ? 'Requested' : 'Request'}
					</button>
				{/if}
			</div>
		{/if}
	</section>
{/if}
