<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { fly } from 'svelte/transition';
	import {
		ArrowRight,
		Check,
		Disc3,
		Download,
		ExternalLink,
		Flag,
		Headphones,
		Loader2,
		RefreshCw,
		Volume2,
		X
	} from 'lucide-svelte';
	import { getApiUrl } from '$lib/api/api-utils';
	import { discoverQueueDeck } from '$lib/stores/discoverQueueDeck.svelte';
	import { deckSampler } from '$lib/stores/deckSampler.svelte';
	import { audioFocus } from '$lib/stores/audioFocus.svelte';
	import { playerStore } from '$lib/stores/player.svelte';
	import { integrationStore } from '$lib/stores/integration';
	import { libraryStore } from '$lib/stores/library';
	import { requestAlbum } from '$lib/queries/downloads/DownloadMutations.svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import HeroBackdrop from '$lib/components/HeroBackdrop.svelte';
	import YouTubeIcon from '$lib/components/YouTubeIcon.svelte';
	import { getQueuePreviewMutation } from '$lib/queries/discover/DiscoverDemand.svelte';

	interface Props {
		youtubeEnabled: boolean;
	}

	let { youtubeEnabled }: Props = $props();

	const deck = discoverQueueDeck;

	let requesting = $state(false);
	let bioExpanded = $state(false);
	let videoOpen = $state(false);
	let ytSearching = $state(false);
	let ytEmbedUrl = $state<string | null>(null);
	let ytError = $state<string | null>(null);
	let ytSearchUrl = $state<string | null>(null);
	let previewController: AbortController | null = null;
	let previewGeneration = 0;
	const previewMutation = getQueuePreviewMutation();

	const current = $derived(deck.current);
	const cardIdentity = $derived(`${deck.requestKey}:${current?.release_group_mbid ?? ''}`);
	const enrichment = $derived(current?.enrichment);
	const enriching = $derived(current != null && !current.enrichment);
	const artistMbid = $derived(current?.artist_mbid || enrichment?.artist_mbid || '');
	const progressText = $derived(
		deck.queue.length > 0 ? `${deck.currentIndex + 1} / ${deck.queue.length}` : ''
	);
	const isRequested = $derived(
		!!current &&
			(('requested' in current && (current as { requested?: boolean }).requested) ||
				libraryStore.isRequested(current.release_group_mbid))
	);
	const releaseYear = $derived(enrichment?.release_date?.slice(0, 4) ?? null);
	const sampling = $derived(
		!!current &&
			deckSampler.activeKey === current.release_group_mbid &&
			deckSampler.status !== 'idle'
	);
	const externalSearchUrl = $derived(
		ytSearchUrl ||
			enrichment?.youtube_search_url ||
			`https://www.youtube.com/results?search_query=${encodeURIComponent(`${current?.artist_name ?? ''} ${current?.album_name ?? ''} music video`)}`
	);

	// one-sound rule (other direction): global playback starting kills deck audio
	$effect(() => {
		if (playerStore.isPlaying) {
			audioFocus.interrupt();
			videoOpen = false;
		}
	});

	// leaving the current item resets its transient panes
	$effect(() => {
		void cardIdentity;
		previewGeneration++;
		previewController?.abort();
		audioFocus.release('deck-video');
		ytSearchUrl = null;
		bioExpanded = false;
		videoOpen = false;
		ytEmbedUrl = null;
		ytError = null;
		ytSearching = false;
	});

	onMount(() => {
		let visible = false;
		const enter = () => {
			if (visible && document.visibilityState === 'visible') {
				void deck.init(() => visible && document.visibilityState === 'visible');
			}
		};
		const onVisibility = () => {
			if (document.visibilityState === 'hidden') deck.destroy();
			else enter();
		};
		const observer = new IntersectionObserver(([entry]) => {
			const entering = entry.isIntersecting && !visible;
			visible = entry.isIntersecting;
			if (entering) enter();
		});
		if (deckEl) observer.observe(deckEl);
		document.addEventListener('visibilitychange', onVisibility);
		window.addEventListener('focus', enter);
		return () => {
			observer.disconnect();
			document.removeEventListener('visibilitychange', onVisibility);
			window.removeEventListener('focus', enter);
		};
	});

	onDestroy(() => {
		previewGeneration++;
		previewController?.abort();
		audioFocus.release('deck-video');
		// Leave the preview playing: it's the app-wide sampler singleton that the
		// floating PreviewWidget picks up so a sample follows you off the Discover
		// page, like every other page that starts one.
		deck.destroy();
	});

	async function openVideo() {
		if (!current || ytSearching) return;
		const mbid = current.release_group_mbid;
		const scope = deck.requestKey;
		const generation = ++previewGeneration;
		const isCurrent = () =>
			generation === previewGeneration &&
			current?.release_group_mbid === mbid &&
			deck.requestKey === scope;
		deckSampler.stop();
		audioFocus.claim('deck-video', () => {
			previewGeneration++;
			previewController?.abort();
			ytSearching = false;
			videoOpen = false;
		});
		if (enrichment?.youtube_url) {
			ytEmbedUrl = enrichment.youtube_url;
			videoOpen = true;
			return;
		}
		previewController?.abort();
		previewController = new AbortController();
		ytSearching = true;
		ytError = null;
		try {
			const data = await previewMutation.mutateAsync({ mbid, signal: previewController.signal });
			if (!isCurrent()) return;
			ytSearchUrl = data.youtube_search_url;
			if (data.status === 'available' && data.youtube_url) {
				ytEmbedUrl = data.youtube_url;
				videoOpen = true;
			} else {
				ytError = data.status;
			}
		} catch {
			if (isCurrent()) ytError = 'request_failed';
		} finally {
			if (isCurrent()) ytSearching = false;
		}
	}

	function closeVideo() {
		videoOpen = false;
		audioFocus.release('deck-video');
	}

	function toggleSample() {
		if (!current) return;
		if (sampling) {
			deckSampler.stop();
			return;
		}
		videoOpen = false;
		void deckSampler.start(current.release_group_mbid, current.artist_name, current.album_name);
	}

	async function handleRequest() {
		if (!current || requesting) return;
		requesting = true;
		try {
			const result = await deckRequest
				.mutateAsync({
					release_group_mbid: current.release_group_mbid,
					artist_name: current.artist_name,
					album_title: current.album_name,
					artist_mbid: current.artist_mbid || undefined
				})
				.catch(() => null);
			if (result?.success) deck.markCurrentRequested();
		} finally {
			requesting = false;
		}
	}

	const deckRequest = requestAlbum();

	function handleAdvance() {
		deckSampler.stop();
		if (deck.isLast) {
			deck.finish();
		} else {
			deck.next();
		}
	}

	function handleIgnore() {
		deckSampler.stop();
		void deck.ignoreCurrent();
	}

	let deckEl = $state<HTMLElement | undefined>();

	function handleGlobalKeydown(event: KeyboardEvent) {
		// arrows drive the deck only when focus is inside it (never steal typing)
		const target = event.target as HTMLElement | null;
		if (target && /^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName)) return;
		if (!deckEl || !target || !deckEl.contains(target)) return;
		if (event.key === 'ArrowRight') {
			event.preventDefault();
			if (!deck.isLast) deck.next();
		} else if (event.key === 'ArrowLeft') {
			event.preventDefault();
			deck.previous();
		}
	}

	function truncate(text: string, max: number): string {
		if (text.length <= max) return text;
		return text.slice(0, max).trimEnd() + '…';
	}

	const RING_R = 17;
	const RING_C = 2 * Math.PI * RING_R;
</script>

<svelte:window onkeydown={handleGlobalKeydown} />

<section
	bind:this={deckEl}
	class="deck group relative overflow-hidden rounded-3xl border border-base-content/10 bg-base-200/40 shadow-[0_8px_40px_oklch(from_var(--color-primary)_l_c_h_/_0.08)]"
	aria-roledescription="carousel"
	aria-label="Discover Queue"
>
	{#if current}
		<HeroBackdrop
			imageUrl={getApiUrl(`/api/v1/covers/release-group/${current.release_group_mbid}?size=500`)}
			opacity={0.16}
			hoverOpacity={0.22}
			blur={22}
			hoverBlur={18}
			position="full"
		/>
	{/if}

	{#if deck.phase === 'loading' || deck.phase === 'idle'}
		<div class="relative grid gap-6 p-6 sm:p-8 lg:grid-cols-[280px_1fr]">
			<div class="skeleton skeleton-shimmer aspect-square w-full max-w-70 rounded-2xl"></div>
			<div class="flex flex-col gap-3 py-2">
				<div class="skeleton skeleton-shimmer h-4 w-40"></div>
				<div class="skeleton skeleton-shimmer h-8 w-72"></div>
				<div class="skeleton skeleton-shimmer h-5 w-52"></div>
				<div class="skeleton skeleton-shimmer h-4 w-full max-w-md"></div>
				<div class="skeleton skeleton-shimmer h-4 w-2/3 max-w-sm"></div>
				<div class="mt-auto flex gap-2">
					<div class="skeleton skeleton-shimmer h-10 w-32 rounded-lg"></div>
					<div class="skeleton skeleton-shimmer h-10 w-28 rounded-lg"></div>
				</div>
			</div>
		</div>
	{:else if deck.phase === 'building' || deck.phase === 'finished'}
		<div class="relative flex flex-col items-center gap-5 px-6 py-14 text-center">
			<div class="flex h-10 items-end justify-center gap-1 pb-1">
				<span class="w-1.5 rounded-full bg-primary animate-equalizer-1" style="height: 60%;"></span>
				<span class="w-1.5 rounded-full bg-primary animate-equalizer-2" style="height: 80%;"></span>
				<span class="w-1.5 rounded-full bg-primary animate-equalizer-3" style="height: 40%;"></span>
				<span
					class="w-1.5 rounded-full bg-primary animate-equalizer-1"
					style="height: 70%; animation-delay: 0.2s;"
				></span>
			</div>
			<div>
				<h3 class="text-xl font-bold">
					{deck.phase === 'finished' ? "That's your queue - fresh one brewing" : 'Discover Queue'}
				</h3>
				<p class="mt-1 text-sm text-base-content/60">
					{deck.phase === 'finished'
						? 'Nice digging. New recommendations are building now.'
						: 'Building your personalised queue…'}
				</p>
			</div>
			<button class="btn btn-ghost btn-sm text-base-content/50" onclick={() => deck.buildNow()}>
				Build now instead
			</button>
		</div>
	{:else if deck.phase === 'error'}
		<div class="relative flex flex-col items-center gap-4 px-6 py-14 text-center">
			<Flag class="h-10 w-10 text-base-content/30" />
			<div>
				<h3 class="text-xl font-bold">We couldn't build your queue</h3>
				<p class="mt-1 text-sm text-base-content/60">{deck.errorMessage}</p>
			</div>
			<div class="flex gap-2">
				<button class="btn btn-primary btn-sm gap-2" onclick={() => deck.retryBuild()}>
					<RefreshCw class="h-4 w-4" /> Retry
				</button>
				<button class="btn btn-ghost btn-sm" onclick={() => deck.buildNow()}>Build anyway</button>
			</div>
		</div>
	{:else if deck.phase === 'empty'}
		<div class="relative flex flex-col items-center gap-4 px-6 py-14 text-center">
			<Disc3 class="h-10 w-10 text-base-content/30" strokeWidth={1.5} />
			<div>
				<h3 class="text-xl font-bold">Nothing to discover right now</h3>
				<p class="mt-1 text-sm text-base-content/60">
					Listen to more music, or try again in a moment.
				</p>
			</div>
			<button class="btn btn-primary btn-sm gap-2" onclick={() => deck.retryBuild()}>
				<RefreshCw class="h-4 w-4" /> Try again
			</button>
		</div>
	{:else if current}
		{#key current.release_group_mbid}
			<div in:fly={{ x: 24, duration: 300 }} class="relative">
				<div class="flex items-start justify-between gap-3 px-6 pt-5 sm:px-8">
					<span
						class="font-mono text-[0.65rem] font-semibold uppercase tracking-widest text-primary/80"
					>
						{current.recommendation_reason}
						{#if current.is_wildcard}
							<span class="badge badge-warning badge-xs ml-2 align-middle">Wildcard</span>
						{/if}
					</span>
					<span
						class="shrink-0 rounded-full border border-base-content/10 bg-base-content/5 px-2 py-0.5 font-mono text-xs text-base-content/50"
					>
						{progressText}
					</span>
				</div>

				<div class="grid gap-6 p-6 pt-3 sm:p-8 sm:pt-3 lg:grid-cols-[280px_1fr]">
					<!-- cover / video pane -->
					<div class="relative mx-auto w-full max-w-70 lg:mx-0">
						{#if videoOpen && ytEmbedUrl}
							<div
								class="relative w-full overflow-hidden rounded-2xl bg-base-300 shadow-xl"
								style="padding-bottom: 56.25%"
							>
								<iframe
									src={ytEmbedUrl}
									title="Album video"
									frameborder="0"
									allow="accelerometer; autoplay; clipboard-write; encrypted-media; gyroscope; picture-in-picture"
									allowfullscreen
									class="absolute inset-0 h-full w-full"
								></iframe>
							</div>
							<button
								class="btn btn-circle btn-xs absolute -right-2 -top-2 border-none bg-base-100 shadow"
								onclick={closeVideo}
								aria-label="Close video"
							>
								<X class="h-3.5 w-3.5" />
							</button>
						{:else}
							<div class="deck-cover relative overflow-hidden rounded-2xl shadow-xl">
								<a
									href={`/album/${current.release_group_mbid}`}
									aria-label="View {current.album_name}"
								>
									<AlbumImage
										mbid={current.release_group_mbid}
										alt={current.album_name}
										size="full"
										requestSize={250}
										responsiveSizes="(max-width: 400px) 70vw, 280px"
										testId="discover-primary-cover"
										lazy={false}
										rounded="none"
										className="aspect-square w-full object-cover"
									/>
								</a>

								{#if !sampling}
									<button
										class="deck-yt-overlay absolute inset-0 flex items-center justify-center"
										onclick={openVideo}
										disabled={ytSearching}
										aria-label="Play music video"
										title="Play video"
									>
										<span
											class="flex h-16 w-16 items-center justify-center rounded-full bg-base-100/85 shadow-lg backdrop-blur-sm transition-transform duration-300"
										>
											{#if ytSearching}
												<Loader2
													class="h-7 w-7 animate-spin"
													style="color: var(--color-youtube);"
												/>
											{:else}
												<YouTubeIcon class="h-8 w-8" />
											{/if}
										</span>
									</button>
								{/if}

								{#if sampling}
									<div
										class="absolute inset-x-0 bottom-0 flex items-center gap-3 bg-gradient-to-t from-black/80 to-transparent p-3 pt-8 text-white"
									>
										<svg viewBox="0 0 40 40" class="h-10 w-10 shrink-0 -rotate-90">
											<circle
												cx="20"
												cy="20"
												r={RING_R}
												fill="none"
												stroke="currentColor"
												stroke-opacity="0.25"
												stroke-width="3"
											/>
											<circle
												cx="20"
												cy="20"
												r={RING_R}
												fill="none"
												stroke="currentColor"
												stroke-width="3"
												stroke-linecap="round"
												stroke-dasharray={RING_C}
												stroke-dashoffset={RING_C * (1 - deckSampler.progress)}
											/>
										</svg>
										<div class="min-w-0 flex-1 text-xs">
											{#if deckSampler.status === 'loading'}
												<p class="font-semibold">Finding samples…</p>
											{:else if deckSampler.currentTrack}
												<p class="truncate font-semibold">
													Sampling {deckSampler.trackIndex + 1}/{deckSampler.tracks.length} ·
													{deckSampler.currentTrack.title}
												</p>
												{#if deckSampler.provider}
													<p class="opacity-60">
														via {deckSampler.provider === 'deezer' ? 'Deezer' : 'iTunes'}
													</p>
												{/if}
											{/if}
										</div>
										<button
											class="btn btn-circle btn-xs border-none bg-white/20 text-white hover:bg-white/30"
											onclick={() => deckSampler.stop()}
											aria-label="Stop sampling"
										>
											<X class="h-3.5 w-3.5" />
										</button>
									</div>
								{/if}
							</div>
						{/if}
					</div>

					<!-- info pane -->
					<div class="flex min-w-0 flex-col">
						<a
							href={`/album/${current.release_group_mbid}`}
							class="w-fit max-w-full truncate text-2xl font-extrabold leading-tight tracking-tight transition-colors hover:text-primary sm:text-3xl"
						>
							{current.album_name}
						</a>
						{#if artistMbid}
							<a
								href={`/artist/${artistMbid}`}
								class="w-fit text-sm font-semibold uppercase tracking-wide text-base-content/60 transition-colors hover:text-primary"
							>
								{current.artist_name}
							</a>
						{:else}
							<span class="text-sm font-semibold uppercase tracking-wide text-base-content/60">
								{current.artist_name}
							</span>
						{/if}

						{#if enriching}
							<div class="mt-4 flex flex-col gap-2">
								<div class="skeleton h-4 w-48 rounded"></div>
								<div class="skeleton h-4 w-64 rounded"></div>
								<div class="skeleton h-4 w-40 rounded"></div>
							</div>
						{:else}
							<div
								class="mt-3 flex flex-wrap items-center gap-x-4 gap-y-1 text-sm text-base-content/60"
							>
								{#if releaseYear}<span>{releaseYear}</span>{/if}
								{#if enrichment?.country}<span>{enrichment.country}</span>{/if}
								{#if enrichment?.listen_count}
									<span class="inline-flex items-center gap-1">
										<Headphones class="h-3.5 w-3.5" />
										{enrichment.listen_count.toLocaleString()} listens
									</span>
								{/if}
								{#if current.in_library}
									<span class="badge badge-success badge-sm gap-1"
										><Check class="h-3 w-3" /> In library</span
									>
								{/if}
							</div>

							{#if enrichment?.tags?.length}
								<div class="mt-3 flex flex-wrap gap-1.5">
									{#each [...new Set(enrichment.tags)].slice(0, 6) as tag (tag)}
										<a
											href={`/genre?name=${encodeURIComponent(tag)}`}
											class="badge badge-ghost badge-sm border-base-content/10 transition-colors hover:border-primary/40 hover:text-primary"
										>
											{tag}
										</a>
									{/each}
								</div>
							{/if}

							{#if enrichment?.artist_description}
								<p class="mt-4 max-w-2xl text-xs leading-relaxed text-base-content/55">
									{bioExpanded
										? enrichment.artist_description
										: truncate(enrichment.artist_description, 280)}
									{#if enrichment.artist_description.length > 280}
										<button
											class="ml-1 cursor-pointer border-none bg-transparent p-0 text-xs text-primary hover:underline"
											onclick={() => (bioExpanded = !bioExpanded)}
										>
											{bioExpanded ? 'less' : 'more'}
										</button>
									{/if}
								</p>
							{/if}
						{/if}
						{#if ytError}
							<p class="mt-2 text-xs text-base-content/60" role="status">
								{ytError === 'request_failed'
									? 'Video lookup failed. Try again.'
									: ytError === 'unavailable'
										? 'Video search is unavailable. You can still search YouTube.'
										: 'No video found for this album.'}
							</p>
						{/if}

						<div class="mt-5 flex flex-col gap-2 pt-1">
							<div class="flex flex-wrap items-center gap-2">
								<button
									class="btn btn-ghost btn-sm gap-2"
									onclick={openVideo}
									disabled={ytSearching}
									title={youtubeEnabled
										? 'Find a music video'
										: 'Look for an existing music video link'}
								>
									{#if ytSearching}<Loader2
											class="h-4 w-4 animate-spin motion-reduce:animate-none"
										/>{:else}<YouTubeIcon class="h-4 w-4" />{/if}
									{ytSearching ? 'Finding video…' : ytError ? 'Retry video' : 'Find video'}
								</button>
								<a
									class="btn btn-ghost btn-sm gap-2"
									href={externalSearchUrl}
									target="_blank"
									rel="noopener noreferrer"
								>
									Search YouTube <ExternalLink class="h-3.5 w-3.5" />
								</a>
								<div class="flex flex-col gap-1">
									<button
										class="btn btn-sm gap-2 border-none bg-base-content/10 hover:bg-base-content/20"
										class:btn-active={sampling}
										onclick={toggleSample}
										disabled={deckSampler.status === 'loading' && sampling}
										title="Play 30-second samples of this album"
									>
										{#if sampling && deckSampler.status === 'loading'}
											<Loader2 class="h-4 w-4 animate-spin" />
										{:else if sampling}
											<X class="h-4 w-4" />
										{:else}
											<Disc3 class="h-4 w-4" />
										{/if}
										{sampling ? 'Stop sample' : 'Sample album'}
									</button>
									<label class="flex items-center gap-1.5 px-1" title="Preview volume">
										<Volume2 class="h-3 w-3 shrink-0 text-base-content/40" />
										<input
											type="range"
											min="0"
											max="1"
											step="0.05"
											value={deckSampler.volume}
											oninput={(e) => deckSampler.setVolume(Number(e.currentTarget.value))}
											class="range range-primary range-xs w-24"
											aria-label="Preview volume"
										/>
									</label>
								</div>

								{#if $integrationStore.download_client && !current.in_library}
									<button
										class="btn btn-primary btn-sm gap-2"
										onclick={handleRequest}
										disabled={requesting || isRequested}
									>
										{#if requesting}
											<Loader2 class="h-4 w-4 animate-spin" />
										{:else if isRequested}
											<Check class="h-4 w-4" />
										{:else}
											<Download class="h-4 w-4" />
										{/if}
										{isRequested ? 'Requested' : 'Request'}
									</button>
								{/if}

								<button class="btn btn-ghost btn-sm gap-2 text-error/80" onclick={handleIgnore}>
									<X class="h-4 w-4" /> Not for me
								</button>
							</div>

							<!-- Kept on its own non-wrapping row so Next never shifts position when the
							     buttons above change width/visibility per recommendation (#403). -->
							<div class="flex items-center gap-2">
								<button class="btn btn-outline btn-sm gap-2" onclick={handleAdvance}>
									{deck.isLast ? 'Finish queue' : 'Next'}
									<ArrowRight class="h-4 w-4" />
								</button>
								<button
									class="btn btn-ghost btn-sm gap-2"
									onclick={() => deck.buildNow()}
									disabled={deck.replacing}
								>
									<RefreshCw
										class={`h-3.5 w-3.5 ${deck.replacing ? 'animate-spin motion-reduce:animate-none' : ''}`}
									/>
									{deck.replacing ? 'Building replacement…' : 'New queue'}
								</button>
							</div>
							{#if deck.errorMessage}<p role="status" class="w-full text-xs text-warning">
									{deck.errorMessage}
								</p>{/if}
						</div>
					</div>
				</div>

				<!-- filmstrip -->
				{#if deck.queue.length > 1}
					<div class="relative border-t border-base-content/5 bg-base-100/30 px-6 py-3 sm:px-8">
						<div
							class="flex items-center gap-2 overflow-x-auto pb-1"
							role="tablist"
							aria-label="Queue items"
						>
							<span
								class="mr-1 shrink-0 text-[0.65rem] font-semibold uppercase tracking-widest text-base-content/35"
							>
								Up next
							</span>
							{#each deck.queue as item, i (item.release_group_mbid)}
								<button
									role="tab"
									aria-selected={i === deck.currentIndex}
									aria-label="{item.album_name} by {item.artist_name}"
									title="{item.album_name} - {item.artist_name}"
									class="deck-strip-item shrink-0 overflow-hidden rounded-lg transition-all duration-200 {i ===
									deck.currentIndex
										? 'ring-2 ring-primary ring-offset-2 ring-offset-base-100'
										: 'opacity-55 hover:opacity-100'}"
									onclick={() => {
										deckSampler.stop();
										deck.jumpTo(i);
									}}
								>
									<AlbumImage
										mbid={item.release_group_mbid}
										alt={item.album_name}
										size="full"
										requestSize={250}
										lazy={i > deck.currentIndex + 4}
										rounded="none"
										className="block h-12 w-12 object-cover"
									/>
								</button>
							{/each}
						</div>
					</div>
				{/if}
			</div>
		{/key}
	{/if}
</section>

<style>
	.deck-cover:hover .deck-yt-overlay span {
		transform: scale(1.08);
	}

	.deck-yt-overlay {
		background: transparent;
		border: none;
		cursor: pointer;
	}

	.deck-yt-overlay span :global(svg) {
		color: var(--color-youtube);
	}

	.deck-strip-item:focus-visible {
		outline: 2px solid var(--color-primary);
		outline-offset: 2px;
	}
</style>
