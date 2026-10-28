<script lang="ts">
	import { Check, Download, Headphones, LoaderCircle, Play, Volume2 } from 'lucide-svelte';
	import { SvelteSet } from 'svelte/reactivity';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import { deckSampler, type SampleEntry } from '$lib/stores/deckSampler.svelte';
	import { libraryStore } from '$lib/stores/library';
	import { requestAlbum } from '$lib/queries/downloads/DownloadMutations.svelte';
	import { albumHrefOrNull } from '$lib/utils/entityRoutes';
	import type { AlbumRef, ChartSection, TopPicksSection } from '$lib/queries/discover/types';
	import { albumRows } from '$lib/utils/sectionRows';
	import LiveUpdatingBadge from '$lib/components/LiveUpdatingBadge.svelte';

	interface Props {
		section: ChartSection | null;
		topPicks: TopPicksSection | null;
		updating?: boolean;
	}

	let { section, topPicks, updating = false }: Props = $props();

	let requesting = $state(false);

	// browse-by-ear pool: listeners-like-you albums + top-pick runners-up
	const pool = $derived.by(() => {
		const seen = new SvelteSet<string>();
		const albums: AlbumRef[] = [];
		for (const item of albumRows(section)) {
			if (item.musicbrainz_id && !seen.has(item.musicbrainz_id)) {
				seen.add(item.musicbrainz_id);
				albums.push(item);
			}
		}
		for (const pick of topPicks?.items.slice(3) ?? []) {
			if (pick.album.musicbrainz_id && !seen.has(pick.album.musicbrainz_id)) {
				seen.add(pick.album.musicbrainz_id);
				albums.push(pick.album);
			}
		}
		return albums.slice(0, 15);
	});

	const activeAlbum = $derived(
		pool.find((a) => a.musicbrainz_id === deckSampler.activeKey && deckSampler.status !== 'idle') ??
			null
	);
	const activeRequested = $derived(
		!!activeAlbum && (activeAlbum.requested || libraryStore.isRequested(activeAlbum.musicbrainz_id))
	);

	function albumEntry(album: AlbumRef): SampleEntry {
		// pool guarantees a non-null mbid (filtered below), so the coalesce never fires
		return {
			key: album.musicbrainz_id ?? '',
			kind: 'album',
			artist: album.artist_name ?? '',
			title: album.title,
			albumMbid: album.musicbrainz_id,
			artistMbid: album.artist_id,
			coverUrl: album.cover_url
		};
	}

	function sample(album: AlbumRef) {
		if (!album.musicbrainz_id) return;
		if (deckSampler.activeKey === album.musicbrainz_id && deckSampler.status !== 'idle') {
			deckSampler.stop();
			return;
		}
		void deckSampler.start(album.musicbrainz_id, album.artist_name ?? '', album.title, {
			artistMbid: album.artist_id,
			coverUrl: album.cover_url
		});
	}

	function playAll() {
		if (pool.length === 0) return;
		// lean-back: hear the whole shelf as a 30s-preview station in the widget
		deckSampler.startStation('Listening Lounge', pool.map(albumEntry));
	}

	const loungeRequest = requestAlbum();

	async function requestActive() {
		if (!activeAlbum?.musicbrainz_id || requesting) return;
		requesting = true;
		try {
			await loungeRequest
				.mutateAsync({
					release_group_mbid: activeAlbum.musicbrainz_id,
					artist_name: activeAlbum.artist_name ?? undefined,
					album_title: activeAlbum.title,
					artist_mbid: activeAlbum.artist_id ?? undefined
				})
				.catch(() => null);
		} finally {
			requesting = false;
		}
	}

	const RING_R = 15;
	const RING_C = 2 * Math.PI * RING_R;
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
</script>

{#if pool.length > 0}
	<Card
		as="section"
		variant="bordered"
		class="relative overflow-hidden bg-gradient-to-br from-secondary/8 via-base-200/50 to-primary/6"
	>
		<div class="mb-2 flex flex-wrap items-center gap-x-4 gap-y-2">
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
				<Button
					variant="accent"
					size="sm"
					icon={Play}
					class="[&_svg]:fill-current"
					onclick={playAll}
				>
					Play all
				</Button>
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
			{#each pool as album (album.musicbrainz_id)}
				{@const isActive =
					album.musicbrainz_id === deckSampler.activeKey && deckSampler.status !== 'idle'}
				{@const href = albumHrefOrNull(album.musicbrainz_id)}
				<div class="w-40 shrink-0 sm:w-48">
					<button
						class="lounge-cover group/card relative w-full overflow-hidden rounded-xl shadow-md transition-all duration-300 {isActive
							? 'ring-2 ring-secondary ring-offset-2 ring-offset-base-100'
							: 'hover:-translate-y-1 hover:shadow-xl'}"
						onclick={() => sample(album)}
						aria-label="{isActive ? 'Stop' : 'Preview'} {album.title} by {album.artist_name}"
						title={isActive ? 'Stop preview' : 'Preview this album'}
					>
						<AlbumImage
							mbid={album.musicbrainz_id || ''}
							alt={album.title}
							size="full"
							requestSize={250}
							lazy={true}
							rounded="none"
							className="block aspect-square w-full object-cover"
							customUrl={album.cover_url || null}
						/>
						<div
							class="absolute inset-0 flex items-center justify-center bg-black/0 transition-colors duration-300 {isActive
								? 'bg-black/35'
								: 'group-hover/card:bg-black/25'}"
						>
							{#if isActive && deckSampler.status === 'loading'}
								<LoaderCircle class="h-8 w-8 animate-spin motion-keep text-white drop-shadow" />
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
						{album.title}
					</svelte:element>
					<p class="truncate text-xs text-base-content/50">{album.artist_name}</p>
				</div>
			{/each}
		</div>

		{#if activeAlbum}
			<div
				class="mt-1 flex items-center gap-3 rounded-xl border border-secondary/15 bg-base-100/60 px-4 py-2 backdrop-blur-sm"
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
				{#if !activeAlbum.in_library}
					<Button
						variant="outline"
						size="xs"
						icon={activeRequested ? Check : Download}
						loading={requesting}
						disabled={activeRequested}
						onclick={requestActive}
					>
						{activeRequested ? 'Requested' : 'Request'}
					</Button>
				{/if}
			</div>
		{/if}
	</Card>
{/if}
