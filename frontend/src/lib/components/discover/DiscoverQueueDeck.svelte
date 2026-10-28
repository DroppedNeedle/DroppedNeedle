<script lang="ts">
	import { coverSrc } from '$lib/api/covers';
	import { onDestroy, onMount } from 'svelte';
	import { fly } from '$lib/ui/motion';
	import {
		ArrowRight,
		Check,
		Disc3,
		Download,
		ExternalLink,
		Flag,
		Headphones,
		LoaderCircle,
		RefreshCw,
		Volume2,
		X
	} from 'lucide-svelte';
	import { discoverQueueDeck } from '$lib/stores/discoverQueueDeck.svelte';
	import { deckSampler } from '$lib/stores/deckSampler.svelte';
	import { audioFocus } from '$lib/stores/audioFocus.svelte';
	import { playerStore } from '$lib/stores/player.svelte';
	import { libraryStore } from '$lib/stores/library';
	import { requestAlbum } from '$lib/queries/downloads/DownloadMutations.svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import HeroBackdrop from '$lib/components/HeroBackdrop.svelte';
	import YouTubeIcon from '$lib/components/YouTubeIcon.svelte';
	import { getQueuePreviewV3Mutation } from '$lib/queries/discover/DiscoverV3Mutations.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

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
	const previewMutation = getQueuePreviewV3Mutation();

	const current = $derived(deck.current);
	const cardIdentity = $derived(`${deck.requestKey}:${current?.musicbrainz_id ?? ''}`);
	const enrichment = $derived(current?.enrichment);
	const enriching = $derived(current != null && !current.enrichment);
	const artistMbid = $derived(current?.artist_id || enrichment?.artist_mbid || '');
	const progressText = $derived(
		deck.queue.length > 0 ? `${deck.currentIndex + 1} / ${deck.queue.length}` : ''
	);
	const isRequested = $derived(
		!!current &&
			(('requested' in current && (current as { requested?: boolean }).requested) ||
				libraryStore.isRequested(current.musicbrainz_id))
	);
	const releaseYear = $derived(enrichment?.release_date?.slice(0, 4) ?? null);
	const sampling = $derived(
		!!current && deckSampler.activeKey === current.musicbrainz_id && deckSampler.status !== 'idle'
	);
	const externalSearchUrl = $derived(
		ytSearchUrl ||
			enrichment?.youtube_search_url ||
			`https://www.youtube.com/results?search_query=${encodeURIComponent(`${current?.artist_name ?? ''} ${current?.title ?? ''} music video`)}`
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
		const mbid = current.musicbrainz_id;
		const scope = deck.requestKey;
		const generation = ++previewGeneration;
		const isCurrent = () =>
			generation === previewGeneration &&
			current?.musicbrainz_id === mbid &&
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
			ytSearchUrl = data.youtube_search_url ?? null;
			if (data.status === 'available' && data.youtube_url) {
				ytEmbedUrl = data.youtube_url ?? null;
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
		void deckSampler.start(current.musicbrainz_id, current.artist_name, current.title);
	}

	async function handleRequest() {
		if (!current || requesting) return;
		requesting = true;
		try {
			const result = await deckRequest
				.mutateAsync({
					release_group_mbid: current.musicbrainz_id,
					artist_name: current.artist_name,
					album_title: current.title,
					artist_mbid: current.artist_id || undefined
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

<div bind:this={deckEl}>
	<Card
		as="section"
		variant="bordered"
		padding="none"
		class="deck group relative overflow-hidden"
		aria-roledescription="carousel"
		aria-label="Discover Queue"
	>
		{#if current}
			<HeroBackdrop
				imageUrl={coverSrc('release-group', current.musicbrainz_id, 500)}
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
					<span class="h-[60%] w-1.5 rounded-full bg-primary animate-equalizer-1"></span>
					<span class="h-[80%] w-1.5 rounded-full bg-primary animate-equalizer-2"></span>
					<span class="h-[40%] w-1.5 rounded-full bg-primary animate-equalizer-3"></span>
					<span
						class="h-[70%] w-1.5 rounded-full bg-primary animate-equalizer-1 [animation-delay:0.2s]"
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
				<Button variant="ghost" size="sm" onclick={() => deck.buildNow()}>Build now instead</Button>
			</div>
		{:else if deck.phase === 'error'}
			<div class="relative flex flex-col items-center gap-4 px-6 py-14 text-center">
				<Flag class="h-10 w-10 text-base-content/30" />
				<div>
					<h3 class="text-xl font-bold">We couldn't build your queue</h3>
					<p class="mt-1 text-sm text-base-content/60">{deck.errorMessage}</p>
				</div>
				<div class="flex gap-2">
					<Button variant="primary" size="sm" icon={RefreshCw} onclick={() => deck.retryBuild()}>
						Retry
					</Button>
					<Button variant="ghost" size="sm" onclick={() => deck.buildNow()}>Build anyway</Button>
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
				<Button variant="primary" size="sm" icon={RefreshCw} onclick={() => deck.retryBuild()}>
					Try again
				</Button>
			</div>
		{:else if current}
			{#key current.musicbrainz_id}
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
									class="relative w-full overflow-hidden rounded-2xl bg-base-300 pb-[56.25%] shadow-xl"
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
								<IconButton
									variant="neutral"
									size="xs"
									icon={X}
									label="Close video"
									class="absolute -right-2 -top-2"
									onclick={closeVideo}
								/>
							{:else}
								<div class="deck-cover relative overflow-hidden rounded-2xl shadow-xl">
									<a href={`/album/${current.musicbrainz_id}`} aria-label="View {current.title}">
										<AlbumImage
											mbid={current.musicbrainz_id}
											alt={current.title}
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
													<LoaderCircle
														class="h-7 w-7 animate-spin text-[var(--color-youtube)] motion-reduce:animate-none"
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
											<IconButton
												variant="neutral"
												size="xs"
												icon={X}
												label="Stop sampling"
												onclick={() => deckSampler.stop()}
											/>
										</div>
									{/if}
								</div>
							{/if}
						</div>

						<!-- info pane -->
						<div class="flex min-w-0 flex-col">
							<a
								href={`/album/${current.musicbrainz_id}`}
								class="w-fit max-w-full truncate text-2xl font-extrabold leading-tight tracking-tight transition-colors hover:text-primary sm:text-3xl"
							>
								{current.title}
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
									<Button
										variant="ghost"
										size="sm"
										loading={ytSearching}
										onclick={openVideo}
										title={youtubeEnabled
											? 'Find a music video'
											: 'Look for an existing music video link'}
									>
										{#if !ytSearching}<YouTubeIcon class="h-4 w-4" />{/if}
										{ytSearching ? 'Finding video…' : ytError ? 'Retry video' : 'Find video'}
									</Button>
									<Button
										variant="ghost"
										size="sm"
										icon={ExternalLink}
										iconPosition="end"
										href={externalSearchUrl}
										target="_blank"
										rel="noopener noreferrer"
									>
										Search YouTube
									</Button>
									<div class="flex flex-col gap-1">
										<Button
											size="sm"
											active={sampling}
											loading={sampling && deckSampler.status === 'loading'}
											icon={sampling ? X : Disc3}
											onclick={toggleSample}
											title="Play 30-second samples of this album"
										>
											{sampling ? 'Stop sample' : 'Sample album'}
										</Button>
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

									{#if !current.in_library}
										<Button
											variant="primary"
											size="sm"
											loading={requesting}
											icon={isRequested ? Check : Download}
											disabled={isRequested}
											onclick={handleRequest}
										>
											{isRequested ? 'Requested' : 'Request'}
										</Button>
									{/if}

									<Button variant="danger-ghost" size="sm" icon={X} onclick={handleIgnore}>
										Not for me
									</Button>
								</div>

								<!-- Kept on its own non-wrapping row so Next never shifts position when the
							     buttons above change width/visibility per recommendation (#403). -->
								<div class="flex items-center gap-2">
									<Button
										variant="outline"
										size="sm"
										icon={ArrowRight}
										iconPosition="end"
										onclick={handleAdvance}
									>
										{deck.isLast ? 'Finish queue' : 'Next'}
									</Button>
									<Button
										variant="ghost"
										size="sm"
										icon={RefreshCw}
										loading={deck.replacing}
										onclick={() => deck.buildNow()}
									>
										{deck.replacing ? 'Building replacement…' : 'New queue'}
									</Button>
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
								{#each deck.queue as item, i (item.musicbrainz_id)}
									<button
										role="tab"
										aria-selected={i === deck.currentIndex}
										aria-label="{item.title} by {item.artist_name}"
										title="{item.title} - {item.artist_name}"
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
											mbid={item.musicbrainz_id}
											alt={item.title}
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
	</Card>
</div>

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
