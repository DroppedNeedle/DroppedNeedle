<!--
	One Daily Mix: a card with four of its covers, its name and how much of
	it the library holds, which opens to the mix in its order. A track found
	by how it sounds carries the embossed FOUND BY SOUND tape; a new one
	carries its reason and Keep (an album request made from the mix). Play
	plays the mix in order, a new track through YouTube while it is set up,
	Start radio a station grown from its library tracks as a playlist's is,
	and Download all files the new albums as one batch.
-->
<script lang="ts">
	import type { DailyMix, MixTrack } from '$lib/features/discover/queries/types';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import DiscoveryBatchModal from '$lib/features/discover/components/DiscoveryBatchModal.svelte';
	import RadioPlayButton from '$lib/features/discover/components/RadioPlayButton.svelte';
	import { createRequestAlbumMutation } from '$lib/features/downloads/queries/DownloadMutations.svelte';
	import { playMix } from '$lib/features/discover/state/mixPlay';
	import { useIntegrationStatus } from '$lib/features/home/queries/home.svelte';
	import { libraryStore } from '$lib/stores/library';
	import { Check, ChevronDown, Disc3, Download, Play } from 'lucide-svelte';
	import CoverPlaceholder from '$lib/ui/CoverPlaceholder.svelte';
	import { tilt } from '$lib/actions/tilt';
	import { slide } from '$lib/ui/motion';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import Tape from '$lib/ui/Tape.svelte';
	import { SvelteSet } from 'svelte/reactivity';

	interface Props {
		mix: DailyMix;
	}

	let { mix }: Props = $props();

	/** Tracks the opened mix shows before "All songs". */
	const FIRST = 12;

	let expanded = $state(false);
	let showAll = $state(false);
	let batchOpen = $state(false);
	let keeping = $state<string | null>(null);

	const request = createRequestAlbumMutation();
	const integrations = useIntegrationStatus();
	const ytConfigured = $derived(!!integrations.current.youtube_api);

	const counts = $derived(
		mix.new > 0 ? `${mix.yours} yours, ${mix.new} new` : `${mix.yours} yours`
	);

	// four covers of different albums, in the mix's order
	const covers = $derived.by(() => {
		const seen = new SvelteSet<string>();
		const picked: MixTrack[] = [];
		for (const track of mix.tracks) {
			const key = track.album.musicbrainz_id ?? track.album.local_id ?? track.album.title;
			if (seen.has(key)) continue;
			seen.add(key);
			picked.push(track);
			if (picked.length === 4) break;
		}
		return picked;
	});

	const shown = $derived(showAll ? mix.tracks : mix.tracks.slice(0, FIRST));

	const length = $derived.by(() => {
		const seconds = mix.tracks.reduce((sum, track) => sum + (track.duration_seconds ?? 0), 0);
		const hours = Math.floor(seconds / 3600);
		const minutes = Math.round((seconds % 3600) / 60);
		if (seconds === 0) return null;
		return hours > 0 ? `${hours} hr ${minutes} min` : `${minutes} min`;
	});

	// the new tracks' albums, once each, for Download all
	const newAlbums = $derived.by(() => {
		const seen = new SvelteSet<string>();
		return mix.tracks
			.filter((track) => track.kind === 'new')
			.map((track) => track.album)
			.filter((album) => {
				const mbid = album.musicbrainz_id;
				if (!mbid || seen.has(mbid)) return false;
				seen.add(mbid);
				return true;
			});
	});
	const requestable = $derived(
		newAlbums.filter(
			(album) =>
				!album.in_library && !album.requested && !libraryStore.isRequested(album.musicbrainz_id)
		).length
	);

	const requested = (track: MixTrack) =>
		!!track.album.requested || libraryStore.isRequested(track.album.musicbrainz_id);

	async function keep(track: MixTrack) {
		const mbid = track.album.musicbrainz_id;
		if (!mbid || keeping) return;
		keeping = mbid;
		try {
			await request.mutateAsync({
				release_group_mbid: mbid,
				artist_name: track.album.artist_name ?? track.artist_name,
				album_title: track.album.title,
				artist_mbid: track.album.artist_id ?? track.artist_mbid ?? undefined,
				made_from: 'mix'
			});
		} catch {
			// the request's own toast says what went wrong
		} finally {
			keeping = null;
		}
	}

	const place = (at: number) => String(at + 1).padStart(2, '0');

	function clock(seconds: number | null | undefined) {
		if (!seconds) return '';
		return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`;
	}

	const TILT_TRANSITION =
		'transform 0.5s var(--ease-spring), box-shadow 0.5s var(--ease-spring), border-color 0.3s ease';
</script>

<div class="w-full">
	<div class="perspective-[1200px]">
		<button
			use:tilt={{ shadowColorVar: 'var(--color-primary)' }}
			type="button"
			class="group relative w-full overflow-hidden rounded-xl border bg-base-200 cursor-pointer text-left
				focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/50 focus-visible:ring-offset-2 focus-visible:ring-offset-base-100
				active:scale-[0.98]
				{expanded ? 'border-action' : 'border-line'}"
			style:transform-style="preserve-3d"
			style:transform="var(--tilt-transform, rotateX(0) rotateY(0))"
			style:box-shadow="var(--tilt-shadow)"
			style:transition={TILT_TRANSITION}
			onclick={() => (expanded = !expanded)}
			aria-expanded={expanded}
			aria-label="{mix.name} - {counts}. {expanded ? 'Collapse' : 'Expand'} to browse."
		>
			<div
				class="pointer-events-none absolute inset-0 rounded-xl"
				style:background="var(--tilt-specular-bg, transparent)"
			></div>

			<div class="relative flex h-32 items-stretch">
				<div class="grid aspect-square h-full shrink-0 grid-cols-2 grid-rows-2">
					{#each covers as track, i (`${track.album.title}-${i}`)}
						<div class="relative overflow-hidden">
							<AlbumImage
								mbid={track.album.musicbrainz_id || ''}
								alt={track.album.title}
								size="sm"
								rounded="none"
								className="w-full h-full"
								customUrl={track.album.cover_url || null}
							/>
						</div>
					{/each}
					{#each Array(Math.max(0, 4 - covers.length)) as _, i (i)}
						<div class="relative overflow-hidden">
							<CoverPlaceholder name={`${mix.name}-${i}`} />
						</div>
					{/each}
				</div>

				<div class="flex min-w-0 flex-1 items-center gap-3 px-4">
					<div class="flex min-w-0 flex-1 flex-col gap-2">
						<h3 class="font-display text-xl leading-none uppercase line-clamp-2 sm:text-2xl">
							{mix.name}
						</h3>
						<span
							class="inline-flex w-fit items-center gap-1 rounded-full border border-edge px-2 py-0.5 font-mono text-[0.6875rem] font-semibold tracking-wide"
						>
							<Disc3 class="h-3 w-3" aria-hidden="true" />
							{counts}
						</span>
					</div>

					<span
						class="flex h-8 w-8 shrink-0 items-center justify-center rounded-full border border-edge transition-colors
							group-hover:border-action group-hover:bg-action group-hover:text-action-content
							{expanded ? 'border-action bg-action text-action-content' : ''}"
					>
						<ChevronDown
							class="h-4 w-4 transition-transform duration-300 {expanded ? 'rotate-180' : ''}"
						/>
					</span>
				</div>
			</div>
		</button>
	</div>
</div>

{#if expanded}
	<div transition:slide={{ duration: 300 }} class="col-span-full overflow-hidden">
		<Card variant="bordered" padding="md" class="mt-1">
			<p class="dn-mix__facts">
				{mix.tracks.length} songs{#if length}&nbsp;- {length}{/if}&nbsp;- quiet to loud and back
			</p>
			<div class="mb-4 flex flex-wrap items-center gap-2">
				<Button
					variant="primary"
					size="sm"
					icon={Play}
					class="[&_svg]:fill-current"
					disabled={mix.yours === 0 && !ytConfigured}
					onclick={() => playMix(mix, ytConfigured)}
				>
					Play
				</Button>
				<RadioPlayButton
					seed={{ seed_type: 'playlist', seed_id: mix.id }}
					playedFrom={{ kind: 'mix', id: mix.id }}
					variant="ghost"
					label="Start radio"
				/>
				{#if requestable > 0}
					<Button
						variant="outline"
						size="sm"
						icon={Download}
						title="Request every new album on this mix"
						onclick={() => (batchOpen = true)}
					>
						Download all ({requestable} album{requestable === 1 ? '' : 's'})
					</Button>
				{/if}
			</div>

			<ol class="dn-mix__tracks" aria-label="{mix.name}, in order">
				{#each shown as track, at (`${at}-${track.local_track_id ?? track.title}`)}
					<li class="dn-mix__row" data-kind={track.kind}>
						<span class="dn-mix__place">{place(at)}</span>
						<div class="min-w-0">
							<p class="dn-mix__title">{track.title}</p>
							<p class="dn-mix__artist">
								{track.artist_name}
								{#if track.kind === 'new' && track.reason}
									<span class="dn-mix__reason">- {track.reason.line}</span>
								{/if}
							</p>
						</div>
						<div class="dn-mix__end">
							{#if track.kind === 'sound'}
								<Tape look="embossed">Found by sound</Tape>
							{:else if track.kind === 'new'}
								<StatusChip tone="action" size="xs">New</StatusChip>
								{#if track.album.in_library}
									<StatusChip icon={Check}>Owned</StatusChip>
								{:else if requested(track)}
									<StatusChip look="waiting">Requested</StatusChip>
								{:else}
									<Button
										variant="outline"
										size="sm"
										loading={keeping === track.album.musicbrainz_id}
										aria-label="Keep {track.album.title}"
										onclick={() => keep(track)}
									>
										Keep
									</Button>
								{/if}
							{/if}
							{#if track.kind !== 'new'}
								<span class="dn-mix__time">{clock(track.duration_seconds)}</span>
							{/if}
						</div>
					</li>
				{/each}
			</ol>
			{#if mix.tracks.length > FIRST}
				<div class="mt-3 flex items-center gap-3">
					<span class="dn-mix__facts">{shown.length} of {mix.tracks.length}</span>
					<Button variant="caps" size="xs" onclick={() => (showAll = !showAll)}>
						{showAll ? 'Fewer' : `All ${mix.tracks.length} songs`}
					</Button>
				</div>
			{/if}
		</Card>
	</div>
{/if}

{#if requestable > 0}
	<DiscoveryBatchModal
		bind:open={batchOpen}
		sectionTitle={mix.name}
		sectionKey="daily_mixes"
		albums={newAlbums}
		madeFrom="mix"
	/>
{/if}

<style>
	.dn-mix__facts {
		margin-bottom: 0.75rem;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 500;
		letter-spacing: 0.12em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.dn-mix__row {
		display: grid;
		grid-template-columns: 2rem minmax(0, 1fr) auto;
		align-items: center;
		gap: 0.75rem;
		padding: 0.5rem 0;
		border-top: 1px solid var(--dn-line);
	}
	.dn-mix__place {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		color: var(--dn-muted);
	}
	.dn-mix__title {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-weight: 600;
		color: var(--color-base-content);
	}
	.dn-mix__artist {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.dn-mix__reason {
		text-transform: none;
		letter-spacing: normal;
	}
	.dn-mix__end {
		display: flex;
		align-items: center;
		gap: 0.75rem;
	}
	.dn-mix__time {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		color: var(--dn-muted);
	}
</style>
