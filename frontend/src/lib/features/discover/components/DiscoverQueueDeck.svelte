<script lang="ts">
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
	import { discoverQueueDeck } from '$lib/features/discover/state/discoverQueueDeck.svelte';
	import { deckSampler } from '$lib/features/discover/state/deckSampler.svelte';
	import { audioFocus } from '$lib/features/player/state/audioFocus.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import { libraryStore } from '$lib/stores/library';
	import { themeStore } from '$lib/stores/theme.svelte';
	import { createRequestAlbumMutation } from '$lib/features/downloads/queries/DownloadMutations.svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import YouTubeIcon from '$lib/media/icons/YouTubeIcon.svelte';
	import { createQueuePreviewV3Mutation } from '$lib/features/discover/queries/DiscoverV3Mutations.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import BarcodeOrnament from '$lib/ui/BarcodeOrnament.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import VinylSleeve from '$lib/ui/VinylSleeve.svelte';

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
	const previewMutation = createQueuePreviewV3Mutation();

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

	const deckRequest = createRequestAlbumMutation();

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

	let deckEl = $state<HTMLElement | null>(null);

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

<div class="dn-deck-lift">
	<Card
		bind:ref={deckEl}
		as="section"
		variant="torn"
		padding="none"
		class="dn-deck"
		aria-roledescription="carousel"
		aria-label="Discover Queue"
	>
		<div class="dn-deck__ink" data-theme={themeStore.paper}>
			{#if deck.phase === 'loading' || deck.phase === 'idle'}
				<div class="grid gap-8 lg:grid-cols-[22rem_1fr]">
					<div class="skeleton skeleton-shimmer aspect-square w-full max-w-80 rounded-md"></div>
					<div class="flex flex-col gap-3 py-2">
						<div class="skeleton skeleton-shimmer h-4 w-40"></div>
						<div class="skeleton skeleton-shimmer h-9 w-80"></div>
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
				<div class="flex flex-col items-center gap-5 py-10 text-center">
					<div class="flex h-10 items-end justify-center gap-1 pb-1" aria-hidden="true">
						<span class="h-[60%] w-1.5 rounded-full bg-primary animate-equalizer-1"></span>
						<span class="h-[80%] w-1.5 rounded-full bg-primary animate-equalizer-2"></span>
						<span class="h-[40%] w-1.5 rounded-full bg-primary animate-equalizer-3"></span>
						<span
							class="h-[70%] w-1.5 rounded-full bg-primary animate-equalizer-1 [animation-delay:0.2s]"
						></span>
					</div>
					<div>
						<h3 class="dn-deck__state-title">
							{deck.phase === 'finished'
								? "That's your queue - fresh one brewing"
								: 'Discover Queue'}
						</h3>
						<p class="mt-2 text-sm text-muted">
							{deck.phase === 'finished'
								? 'Nice digging. New recommendations are building now.'
								: 'Building your personalised queue…'}
						</p>
					</div>
					<Button variant="paper" size="sm" onclick={() => deck.buildNow()}
						>Build now instead</Button
					>
				</div>
			{:else if deck.phase === 'error'}
				<div class="flex flex-col items-center gap-4 py-10 text-center">
					<Flag class="h-10 w-10 text-muted" aria-hidden="true" />
					<div>
						<h3 class="dn-deck__state-title">We couldn't build your queue</h3>
						<p class="mt-2 text-sm text-muted">{deck.errorMessage}</p>
					</div>
					<div class="flex gap-2">
						<Button variant="primary" size="sm" icon={RefreshCw} onclick={() => deck.retryBuild()}>
							Retry
						</Button>
						<Button variant="paper" size="sm" onclick={() => deck.buildNow()}>Build anyway</Button>
					</div>
				</div>
			{:else if deck.phase === 'empty'}
				<div class="flex flex-col items-center gap-4 py-10 text-center">
					<Disc3 class="h-10 w-10 text-muted" strokeWidth={1.5} aria-hidden="true" />
					<div>
						<h3 class="dn-deck__state-title">Nothing to discover right now</h3>
						<p class="mt-2 text-sm text-muted">Listen to more music, or try again in a moment.</p>
					</div>
					<Button variant="primary" size="sm" icon={RefreshCw} onclick={() => deck.retryBuild()}>
						Try again
					</Button>
				</div>
			{:else if current}
				{#key current.musicbrainz_id}
					<div in:fly={{ x: 24, duration: 300 }}>
						<div class="dn-deck__top">
							<p class="dn-deck__tapes">
								<span class="dn-deck__tape">{current.recommendation_reason}</span>
								{#if current.is_wildcard}
									<span class="dn-deck__tape dn-deck__tape--ink">Wildcard</span>
								{/if}
							</p>
							<div class="dn-deck__count">
								<BarcodeOrnament class="dn-deck__barcode" />
								<span class="dn-deck__counter">{progressText}</span>
							</div>
						</div>

						<div class="dn-deck__body">
							<!-- cover / video pane -->
							<div class="dn-deck__media">
								{#if videoOpen && ytEmbedUrl}
									<div
										class="relative w-full overflow-hidden rounded-md bg-base-300 pb-[56.25%] shadow-xl"
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
									<VinylSleeve class="dn-deck__sleeve">
										<div class="deck-cover relative">
											<a
												href={withBasePath(`/album/${current.musicbrainz_id}`)}
												aria-label="View {current.title}"
											>
												<AlbumImage
													mbid={current.musicbrainz_id}
													alt={current.title}
													size="full"
													requestSize={500}
													responsiveSizes="(max-width: 480px) 85vw, 300px"
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
														class="flex h-16 w-16 items-center justify-center rounded-full bg-white/90 shadow-lg transition-transform duration-300"
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
													class="absolute inset-x-0 bottom-0 flex items-center gap-3 bg-gradient-to-t from-black/85 to-transparent p-3 pt-8 text-white"
												>
													<svg
														viewBox="0 0 40 40"
														class="h-10 w-10 shrink-0 -rotate-90"
														aria-hidden="true"
													>
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
																<p class="text-white/75">
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
									</VinylSleeve>
								{/if}
							</div>

							<!-- info pane -->
							<div class="flex min-w-0 flex-col">
								<a
									href={withBasePath(`/album/${current.musicbrainz_id}`)}
									class="dn-deck__title w-fit max-w-full"
								>
									{current.title}
								</a>
								{#if artistMbid}
									<a href={withBasePath(`/artist/${artistMbid}`)} class="dn-deck__artist w-fit">
										{current.artist_name}
									</a>
								{:else}
									<span class="dn-deck__artist">{current.artist_name}</span>
								{/if}

								{#if enriching}
									<div class="mt-4 flex flex-col gap-2">
										<div class="skeleton h-4 w-48 rounded"></div>
										<div class="skeleton h-4 w-64 rounded"></div>
										<div class="skeleton h-4 w-40 rounded"></div>
									</div>
								{:else}
									<div class="dn-deck__facts">
										{#if releaseYear}<span>{releaseYear}</span>{/if}
										{#if enrichment?.country}<span>{enrichment.country}</span>{/if}
										{#if enrichment?.listen_count}
											<span class="inline-flex items-center gap-1.5">
												<Headphones class="h-3.5 w-3.5" aria-hidden="true" />
												{enrichment.listen_count.toLocaleString()} listens
											</span>
										{/if}
										{#if current.in_library}
											<StatusChip tone="action" size="xs" icon={Check}>In library</StatusChip>
										{/if}
									</div>

									{#if enrichment?.tags?.length}
										<div class="mt-3.5 flex flex-wrap gap-1.5">
											{#each [...new Set(enrichment.tags)].slice(0, 6) as tag (tag)}
												<a
													href={withBasePath(`/genre?name=${encodeURIComponent(tag)}`)}
													class="dn-deck__tag"
												>
													{tag}
												</a>
											{/each}
										</div>
									{/if}

									{#if enrichment?.artist_description}
										<p class="mt-4 max-w-3xl text-sm leading-relaxed text-muted">
											{bioExpanded
												? enrichment.artist_description
												: truncate(enrichment.artist_description, 280)}
											{#if enrichment.artist_description.length > 280}
												<button
													class="ml-1 cursor-pointer border-none bg-transparent p-0 text-sm font-semibold text-primary underline-offset-2 hover:underline"
													onclick={() => (bioExpanded = !bioExpanded)}
												>
													{bioExpanded ? 'less' : 'more'}
												</button>
											{/if}
										</p>
									{/if}
								{/if}
								{#if ytError}
									<p class="mt-2 text-xs text-muted" role="status">
										{ytError === 'request_failed'
											? 'Video lookup failed. Try again.'
											: ytError === 'unavailable'
												? 'Video search is unavailable. You can still search YouTube.'
												: 'No video found for this album.'}
									</p>
								{/if}

								<div class="mt-auto flex flex-col gap-3 pt-6">
									<div class="flex flex-wrap items-start gap-2">
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
												variant="paper"
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
												<Volume2 class="h-3 w-3 shrink-0 text-muted" aria-hidden="true" />
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
											variant="paper"
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
					</div>
				{/key}
			{/if}
		</div>

		<!-- filmstrip -->
		{#if current && deck.phase !== 'loading' && deck.phase !== 'idle' && deck.queue.length > 1}
			<div class="dn-deck__band">
				<span class="dn-deck__band-title" aria-hidden="true">Up next</span>
				<div class="dn-deck__strip" role="tablist" aria-label="Queue items">
					{#each deck.queue as item, i (item.musicbrainz_id)}
						<button
							role="tab"
							aria-selected={i === deck.currentIndex}
							aria-label="{item.title} by {item.artist_name}"
							title="{item.title} - {item.artist_name}"
							class="deck-strip-item"
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
								className="block h-14 w-14 object-cover"
							/>
						</button>
					{/each}
				</div>
			</div>
		{/if}
	</Card>
</div>

<style>
	/* the torn edges come from a mask, so the lift is a shadow on a wrapper */
	.dn-deck-lift {
		filter: drop-shadow(0 14px 22px rgb(0 0 0 / 0.35));
	}
	:global([data-theme='light']) .dn-deck-lift {
		filter: drop-shadow(0 10px 18px rgb(14 46 29 / 0.16));
	}

	/* the paper's own colours: the sheet switches to its own theme so the
	   controls on it are measured for that stock, but keeps the card's stock */
	.dn-deck__ink {
		background: transparent;
		color: var(--dn-paper-content);
		padding: 2rem 2.25rem 2.25rem;
	}

	.dn-deck__state-title {
		font-family: var(--font-display);
		font-weight: 400;
		font-size: 1.75rem;
		line-height: 1;
		text-transform: uppercase;
		font-synthesis: none;
	}

	.dn-deck__top {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1rem;
		margin-bottom: 1.5rem;
	}

	.dn-deck__tapes {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.5rem 0.25rem;
		margin-left: -0.75rem;
	}

	/* a strip of tape stuck on the paper, its ends torn */
	.dn-deck__tape {
		display: inline-block;
		padding: 0.4rem 1.1rem;
		background-color: color-mix(in oklab, var(--dn-action) 88%, transparent);
		color: var(--dn-action-content);
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 700;
		letter-spacing: 0.18em;
		text-transform: uppercase;
		rotate: -2.5deg;
		box-shadow: 0 1px 3px rgb(0 0 0 / 0.18);
		clip-path: polygon(
			0 6%,
			3% 0,
			97% 4%,
			100% 0,
			99% 50%,
			100% 100%,
			96% 94%,
			2% 100%,
			0 92%,
			1.5% 50%
		);
	}
	.dn-deck__tape--ink {
		background-color: var(--dn-paper-content);
		color: var(--dn-paper);
		rotate: 3deg;
		margin-left: -0.25rem;
	}

	.dn-deck__count {
		display: flex;
		flex: none;
		flex-direction: column;
		align-items: flex-end;
		gap: 0.5rem;
	}
	.dn-deck__count :global(.dn-deck__barcode) {
		height: 2.25rem;
	}
	.dn-deck__counter {
		padding: 0.2rem 0.75rem;
		border: 1.5px solid currentColor;
		border-radius: 9999px;
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 600;
		letter-spacing: 0.08em;
		white-space: nowrap;
	}

	.dn-deck__body {
		display: grid;
		gap: 2.5rem;
		grid-template-columns: 26rem minmax(0, 1fr);
	}

	.dn-deck__media {
		position: relative;
	}
	.dn-deck__media :global(.dn-deck__sleeve) {
		width: 100%;
	}

	.dn-deck__title {
		display: -webkit-box;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
		overflow: hidden;
		font-size: 2.5rem;
		font-weight: 800;
		line-height: 1.05;
		letter-spacing: -0.02em;
		transition: color 150ms;
	}
	.dn-deck__title:hover,
	a.dn-deck__artist:hover {
		color: var(--color-primary);
	}

	.dn-deck__artist {
		margin-top: 0.5rem;
		font-size: 0.9375rem;
		font-weight: 700;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		transition: color 150ms;
	}

	.dn-deck__facts {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.25rem 1.25rem;
		margin-top: 0.875rem;
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 600;
		letter-spacing: 0.06em;
		color: var(--dn-muted);
	}

	.dn-deck__tag {
		padding: 0.2rem 0.7rem;
		border: 1.5px solid currentColor;
		border-radius: 9999px;
		font-size: 0.75rem;
		font-weight: 600;
		transition:
			background-color 150ms,
			color 150ms;
	}
	.dn-deck__tag:hover {
		background-color: var(--dn-paper-content);
		color: var(--dn-paper);
	}

	/* the dark band along the bottom: Up next and the queue's covers */
	.dn-deck__band {
		display: flex;
		align-items: center;
		gap: 1.25rem;
		padding: 1rem 2.25rem calc(1rem + var(--tear, 0.75rem));
		background-color: var(--dn-paper-tile);
		color: var(--dn-paper-tile-content);
	}
	.dn-deck__band-title {
		flex: none;
		font-family: var(--font-display);
		font-size: 1.625rem;
		line-height: 1;
		text-transform: uppercase;
		color: var(--dn-action);
		font-synthesis: none;
	}
	.dn-deck__strip {
		display: flex;
		align-items: center;
		gap: 0.625rem;
		min-width: 0;
		padding: 0.25rem;
		overflow-x: auto;
		scrollbar-width: thin;
	}

	.deck-strip-item {
		flex: none;
		overflow: hidden;
		border-radius: 0.25rem;
		opacity: 0.62;
		transition:
			opacity 200ms,
			box-shadow 200ms;
	}
	.deck-strip-item:hover {
		opacity: 1;
	}
	.deck-strip-item[aria-selected='true'] {
		opacity: 1;
		box-shadow:
			0 0 0 2px var(--dn-paper-tile),
			0 0 0 4px var(--dn-action);
	}
	.deck-strip-item:focus-visible {
		outline: 2px solid var(--dn-focus);
		outline-offset: 4px;
	}

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

	@media (max-width: 1279px) {
		.dn-deck__body {
			grid-template-columns: 20rem minmax(0, 1fr);
			gap: 2rem;
		}
		.dn-deck__title {
			font-size: 2rem;
		}
	}

	@media (max-width: 1023px) {
		.dn-deck__body {
			grid-template-columns: minmax(0, 1fr);
		}
		.dn-deck__media {
			width: min(100%, 24rem);
			margin-inline: auto;
		}
	}

	@media (max-width: 639px) {
		.dn-deck__ink {
			padding: 1.5rem 1.125rem 1.5rem;
		}
		.dn-deck__top {
			margin-bottom: 1rem;
		}
		.dn-deck__tapes {
			margin-left: -0.375rem;
		}
		.dn-deck__tape {
			padding: 0.3rem 0.75rem;
			font-size: 0.6875rem;
			letter-spacing: 0.12em;
		}
		.dn-deck__title {
			font-size: 1.625rem;
		}
		.dn-deck__band {
			gap: 0.75rem;
			padding-inline: 1.125rem;
		}
		.dn-deck__band-title {
			font-size: 1.25rem;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.dn-deck__tape {
			rotate: none;
		}
	}

	@media (prefers-contrast: more) {
		.dn-deck__tape {
			clip-path: none;
			box-shadow: none;
			border: 1px solid currentColor;
		}
	}
</style>
