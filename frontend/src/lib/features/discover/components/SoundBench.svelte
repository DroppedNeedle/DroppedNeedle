<!--
	By ear: the sound bench, the one instrument on Discover. A metal plate
	with the embossed "Found by sound" label on its top edge. Left, what it is
	tuned to: a library track (the person's most played of the last 30 days
	to start with) and Change track. Middle, the scope drawing the tuned
	track's stored sound over the seed's, and the tuner with the tracks that
	sound like the seed set out as stations by their distance. Right, the
	How close meter, the Tune knob that steps from station to station, the
	tuned track in words with its closeness, and Start radio, a station
	grown from the tuned track. While that station plays, the tray steers it
	(More like this, Not this) and stops it, and the white trace follows the
	player. With sound analysis off the bench, its heading included, is not
	drawn at all.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import { AudioWaveform, Ban, Pause, Plus, Radio } from 'lucide-svelte';
	import { ApiError } from '$lib/api/errors';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Knob from '$lib/ui/Knob.svelte';
	import Meter from '$lib/ui/Meter.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import Panel from '$lib/ui/Panel.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import ServiceErrorState from '$lib/ui/ServiceErrorState.svelte';
	import Scope from '$lib/ui/Scope.svelte';
	import Tape from '$lib/ui/Tape.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import TunerScale from '$lib/ui/TunerScale.svelte';
	import { useIntegrationStatus } from '$lib/features/home/queries/home.svelte';
	import {
		isStationItem,
		launchRadio,
		playingStation,
		steerRadio
	} from '$lib/features/player/playback/launchRadio';
	import { tryGetAudioEngine } from '$lib/features/player/playback/audioElement';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import {
		BENCH_CUT_OFF,
		getBenchTrackSearchQuery,
		getSimilarTracksQuery,
		getTrackSoundQuery,
		soundOff,
		type BenchTrack,
		type Closeness,
		type SimilarTrack
	} from '../queries/soundBench.svelte';
	import type { SoundBench as Bench } from '../queries/types';

	interface Props {
		bench: Bench;
		/** Drawn above the bench, and gone with it. */
		head?: Snippet;
	}

	let { bench, head }: Props = $props();
	/** The closeness word on a station's button. */
	const WORDS: Record<Closeness, string> = {
		very_close: 'very close',
		close: 'close',
		further: 'a little like it'
	};
	/** The tray's line, in front of the seed's title. */
	const SOUNDS: Record<Closeness, string> = {
		very_close: 'Sounds very close to',
		close: 'Sounds close to',
		further: 'Sounds a little like'
	};

	// The seed starts as the server's pick and changes only by Change track.
	let picked = $state<BenchTrack | null>(null);
	const seed = $derived(picked ?? bench.seed ?? null);

	const similarQuery = getSimilarTracksQuery(() => seed?.track_id ?? null);
	const stations = $derived<SimilarTrack[]>(similarQuery.data?.tracks ?? []);
	const off = $derived(soundOff(similarQuery.error));

	let tunedId = $state<string | null>(null);
	const tuned = $derived(stations.find((s) => s.track_id === tunedId) ?? stations[0] ?? null);
	const tunedAt = $derived(tuned ? stations.indexOf(tuned) : 0);

	const version = () => bench.analysis_version;
	const seedSound = getTrackSoundQuery(() => seed?.track_id ?? null, version);
	const tunedSound = getTrackSoundQuery(() => tuned?.track_id ?? null, version);
	const seedMeasures = $derived(seedSound.data?.measures ?? []);
	// The white trace keeps its last shape while the next one loads, so it morphs.
	let shownMeasures = $state<number[] | null>(null);
	$effect(() => {
		const next = tunedSound.data?.measures;
		if (next) shownMeasures = next;
		else if (!tuned) shownMeasures = null;
	});

	const nameOf = (track: { title: string; artist_name: string }) =>
		`${track.title} by ${track.artist_name}`;
	const length = (seconds: number | null | undefined) => {
		if (!seconds || seconds <= 0) return null;
		const whole = Math.round(seconds);
		return `${Math.floor(whole / 60)}:${String(whole % 60).padStart(2, '0')}`;
	};

	function tune(id: string) {
		tunedId = id;
	}

	// ---------- radio ----------

	const integrations = useIntegrationStatus();
	let starting = $state(false);
	/** The track the bench's playing station grew from. */
	let stationFrom = $state<string | null>(null);
	const playing = $derived.by(() => {
		const station = playingStation();
		return !!stationFrom && station?.seed_type === 'track' && station.seed_id === stationFrom;
	});

	async function startRadio() {
		if (!tuned || starting) return;
		starting = true;
		try {
			const started = await launchRadio(
				{ seed_type: 'track', seed_id: tuned.track_id },
				!!integrations.current.youtube_api
			);
			if (started) stationFrom = tuned.track_id;
		} finally {
			starting = false;
		}
	}

	// Steering works on the station's own library track playing now.
	const steerable = $derived.by(() => {
		const item = playing ? playerStore.currentQueueItem : null;
		return item && isStationItem(item) && item.sourceType === 'local' ? item : null;
	});

	function steer(kind: 'more_like' | 'not_this') {
		if (steerable) void steerRadio(kind, steerable);
	}

	const pulse = $derived(
		playing && playerStore.isPlaying && playerStore.nowPlaying?.sourceType !== 'youtube'
			? () => tryGetAudioEngine()?.getFrequencyData() ?? null
			: null
	);

	// ---------- change track ----------

	/** How long typing rests before the library is searched, ms. */
	const SEARCH_REST_MS = 250;
	let choosing = $state(false);
	let search = $state('');
	let searched = $state('');
	$effect(() => {
		const text = choosing ? search : '';
		const wait = setTimeout(() => (searched = text), SEARCH_REST_MS);
		return () => clearTimeout(wait);
	});
	const searchQuery = getBenchTrackSearchQuery(() => searched);
	type Found = NonNullable<typeof searchQuery.data>['items'][number];

	function choose(track: Found) {
		picked = {
			track_id: track.id,
			title: track.title,
			artist_name: track.artist_name,
			album_id: track.album_id,
			album_title: track.album_title,
			duration_seconds: track.duration_seconds ? Math.round(track.duration_seconds) : null
		};
		tunedId = null;
		choosing = false;
		search = '';
	}
</script>

{#if !off}
	{@render head?.()}
	<Panel as="section" look="metal" padding="lg" class="dn-bench" aria-labelledby="dn-bench-title">
		<Tape look="embossed" class="dn-bench__tape">Found by sound</Tape>

		<div class="dn-bench__plate">
			<AudioWaveform class="dn-bench__mark" aria-hidden="true" />
			<h2 id="dn-bench-title" class="dn-bench__title">By ear</h2>
			<p class="dn-bench__lede">More like one track, found by how your own files sound.</p>
			<hr class="dn-bench__rule" />
			<p class="dn-bench__label">Tuned to</p>
			{#if seed}
				<div class="dn-bench__seed">
					<AlbumImage
						albumId={seed.album_id}
						alt={seed.album_title ?? seed.title}
						size="sm"
						rounded="sm"
						className="dn-bench__seed-cover"
					/>
					<div class="min-w-0">
						<p class="dn-bench__seed-title">{seed.title}</p>
						<p class="dn-bench__small">{seed.artist_name}</p>
						<p class="dn-bench__small">
							{#if length(seed.duration_seconds)}{length(seed.duration_seconds)} -
							{/if}On your shelves
						</p>
					</div>
				</div>
			{:else}
				<p class="dn-bench__note">Pick one of your tracks to tune to.</p>
			{/if}
			<Button variant="caps" size="sm" onclick={() => (choosing = true)}>Change track</Button>
			<p class="dn-bench__model">
				Sound model {bench.model}<br />analysis version {bench.analysis_version}
			</p>
		</div>

		<div class="dn-bench__middle">
			{#if seed}
				<Scope
					class="dn-bench__scope"
					label={tuned
						? `How ${seed.title} and ${tuned.title} sound, drawn over each other`
						: `How ${seed.title} sounds`}
					seed={seedMeasures}
					seedKey={seed.track_id}
					tuned={tuned ? shownMeasures : null}
					{pulse}
				/>
				<p class="dn-bench__legend" aria-hidden="true">
					<span class="dn-bench__key" data-trace="seed">{seed.title}</span>
					{#if tuned}<span class="dn-bench__key" data-trace="tuned">{tuned.title}</span>{/if}
					<span class="dn-bench__legend-note">200 measures a track, from your files</span>
				</p>
				<QueryBoundary
					query={similarQuery}
					what="the tracks that sound like it"
					isEmpty={(data) => data.tracks.length === 0}
				>
					{#snippet children(data)}
						<TunerScale
							label="Tracks that sound like {seed.title}"
							stations={data.tracks.map((s) => ({
								...s,
								id: s.track_id,
								name: `${nameOf(s)}, ${WORDS[s.closeness]}`
							}))}
							tuned={tuned?.track_id ?? null}
							ontune={tune}
							max={BENCH_CUT_OFF}
						>
							{#snippet cover(station)}
								<AlbumImage albumId={station.album_id} alt="" size="xs" rounded="full" />
							{/snippet}
						</TunerScale>
					{/snippet}
					{#snippet empty()}
						<p class="dn-bench__note">Nothing in your library sounds close to this one yet.</p>
					{/snippet}
					{#snippet loading()}
						<div class="dn-bench__tuner-wait" role="status" aria-label="Loading stations"></div>
					{/snippet}
					{#snippet failed(error, retry)}
						{#if error instanceof ApiError && error.status === 404}
							<p class="dn-bench__note">This track has not been analysed yet.</p>
						{:else if !soundOff(error)}
							<ServiceErrorState {error} what="the tracks that sound like it" onretry={retry} />
						{/if}
					{/snippet}
				</QueryBoundary>
			{/if}
		</div>

		<div class="dn-bench__right">
			{#if tuned && seed}
				<div class="dn-bench__dials">
					<Meter
						label="{tuned.title}: {SOUNDS[tuned.closeness].toLowerCase()} {seed.title}"
						value={1 - Math.min(1, tuned.distance / BENCH_CUT_OFF)}
						caption="How close"
					/>
					<Knob
						label="Tune"
						size="sm"
						lit="sound"
						min={0}
						max={Math.max(stations.length - 1, 0)}
						dots={Math.max(stations.length, 2)}
						value={tunedAt}
						onchange={(at) => tune(stations[at]?.track_id ?? tuned.track_id)}
						format={(at) => (stations[at] ? nameOf(stations[at]) : '')}
					/>
				</div>
				<Panel look="tray" padding="sm" class="dn-bench__tray">
					<div class="dn-bench__tuned">
						<AlbumImage
							albumId={tuned.album_id}
							alt={tuned.album_title ?? tuned.title}
							size="sm"
							rounded="sm"
							className="dn-bench__tuned-cover"
						/>
						<div class="min-w-0">
							<p class="dn-bench__tuned-title">{tuned.title}</p>
							<p class="dn-bench__small">{tuned.artist_name}</p>
						</div>
					</div>
					<p class="dn-bench__close" aria-live="polite">
						<AudioWaveform class="dn-bench__close-mark" aria-hidden="true" />
						<span>{SOUNDS[tuned.closeness]} {seed.title}</span>
					</p>
				</Panel>
				{#if playing}
					{#if steerable}
						<div class="dn-bench__steer">
							<Button variant="outline" size="sm" icon={Plus} onclick={() => steer('more_like')}>
								More like this
							</Button>
							<Button variant="outline" size="sm" icon={Ban} onclick={() => steer('not_this')}>
								Not this
							</Button>
						</div>
					{/if}
					<Button variant="caps" size="sm" icon={Pause} onclick={() => playerStore.stop()}>
						Stop radio
					</Button>
				{:else}
					<Button
						variant="primary"
						size="display-sm"
						icon={Radio}
						class="w-full"
						loading={starting}
						onclick={startRadio}
					>
						Start radio
					</Button>
				{/if}
			{/if}
		</div>
	</Panel>
{/if}

<Modal bind:open={choosing} title="Change track" size="md" closeButton>
	<TextField label="Find one of your tracks" type="search" bind:value={search} />
	{#if searched.trim().length >= 2}
		<QueryBoundary query={searchQuery} what="your tracks">
			{#snippet children(page)}
				<ul class="dn-bench__found">
					{#each page.items as track (track.id)}
						<li>
							<Button variant="ghost" size="sm" class="w-full" onclick={() => choose(track)}>
								<span class="dn-bench__found-row">
									<span class="truncate font-semibold">{track.title}</span>
									<span class="truncate text-muted">{track.artist_name}</span>
								</span>
							</Button>
						</li>
					{/each}
				</ul>
			{/snippet}
			{#snippet empty()}
				<p class="dn-bench__note mt-3">None of your tracks match that.</p>
			{/snippet}
		</QueryBoundary>
	{/if}
</Modal>

<style>
	:global(.dn-bench) {
		display: grid;
		grid-template-columns: minmax(12rem, 15rem) minmax(0, 1fr) minmax(15rem, 18rem);
		gap: 1.75rem;
		align-items: start;
	}
	:global(.dn-bench__tape) {
		position: absolute;
		top: -0.875rem;
		right: 3.5rem;
	}

	.dn-bench__plate {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 0.5rem;
	}
	.dn-bench__plate :global(.dn-bench__mark) {
		width: 2rem;
		height: 2rem;
		color: var(--color-secondary);
	}
	.dn-bench__title {
		margin: 0;
		font-family: var(--font-display);
		font-size: 2.75rem;
		line-height: 1;
		text-transform: uppercase;
		color: var(--color-secondary);
	}
	.dn-bench__lede,
	.dn-bench__label,
	.dn-bench__small,
	.dn-bench__model,
	.dn-bench__legend {
		font-family: var(--font-mono);
		text-transform: uppercase;
		letter-spacing: 0.14em;
	}
	.dn-bench__lede {
		margin: 0;
		font-size: 0.6875rem;
		line-height: 1.7;
		color: var(--color-muted);
	}
	.dn-bench__rule {
		width: 100%;
		margin: 0.5rem 0;
		border: 0;
		border-top: 1px solid var(--dn-line);
	}
	.dn-bench__label {
		margin: 0;
		font-size: 0.6875rem;
		font-weight: 600;
	}
	.dn-bench__seed,
	.dn-bench__tuned {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		min-width: 0;
	}
	.dn-bench__seed :global(.dn-bench__seed-cover),
	.dn-bench__tuned :global(.dn-bench__tuned-cover) {
		width: 3.5rem;
		height: 3.5rem;
		flex: none;
	}
	.dn-bench__seed-title,
	.dn-bench__tuned-title {
		margin: 0;
		font-weight: 600;
		overflow-wrap: anywhere;
	}
	.dn-bench__small {
		margin: 0;
		font-size: 0.625rem;
		color: var(--color-muted);
	}
	.dn-bench__note {
		margin: 0;
		font-size: 0.875rem;
		color: var(--color-muted);
	}
	.dn-bench__model {
		margin: 0.75rem 0 0;
		font-size: 0.625rem;
		line-height: 1.6;
		color: var(--color-muted);
	}

	.dn-bench__middle {
		display: flex;
		flex-direction: column;
		gap: 0.625rem;
		min-width: 0;
	}
	.dn-bench__middle :global(.dn-bench__scope) {
		height: 15rem;
	}
	.dn-bench__legend {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.375rem 1.25rem;
		margin: 0;
		font-size: 0.625rem;
		color: var(--color-muted);
	}
	.dn-bench__key {
		display: inline-flex;
		align-items: center;
		gap: 0.5rem;
		color: var(--color-base-content);
	}
	.dn-bench__key::before {
		content: '';
		width: 1.25rem;
		height: 2px;
		border-radius: 1px;
	}
	.dn-bench__key[data-trace='seed']::before {
		background: var(--color-secondary);
	}
	.dn-bench__key[data-trace='tuned']::before {
		background: currentColor;
	}
	.dn-bench__legend-note {
		margin-left: auto;
	}

	.dn-bench__right {
		display: flex;
		flex-direction: column;
		gap: 1rem;
	}
	.dn-bench__dials {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1rem;
	}
	.dn-bench__right :global(.dn-bench__tray) {
		display: flex;
		flex-direction: column;
		gap: 0.875rem;
	}
	.dn-bench__close {
		display: flex;
		align-items: flex-start;
		gap: 0.5rem;
		margin: 0;
		font-size: 0.875rem;
	}
	.dn-bench__close :global(.dn-bench__close-mark) {
		width: 1rem;
		height: 1rem;
		flex: none;
		margin-top: 0.125rem;
		color: var(--color-secondary);
	}
	.dn-bench__steer {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: 0.5rem;
	}

	.dn-bench__tuner-wait {
		height: 5.5rem;
		border-radius: 0.75rem;
		background: var(--dn-vinyl);
	}
	.dn-bench__found {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		margin: 0.75rem 0 0;
		padding: 0;
		list-style: none;
	}
	.dn-bench__found-row {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		min-width: 0;
		width: 100%;
		text-align: left;
	}

	/* a phone stacks the bench: plate, scope, tuner, the dials side by side, the tray */
	@media (max-width: 1023px) {
		:global(.dn-bench) {
			grid-template-columns: minmax(0, 1fr);
			gap: 1.25rem;
		}
		:global(.dn-bench__tape) {
			right: 1.5rem;
		}
		.dn-bench__title {
			font-size: 2.25rem;
		}
		.dn-bench__middle :global(.dn-bench__scope) {
			height: 10rem;
		}
		.dn-bench__dials {
			justify-content: space-around;
		}
	}
</style>
