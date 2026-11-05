<script lang="ts">
	import { Play, Shuffle, Sparkles, ArrowRight } from 'lucide-svelte';
	import { getLocalStatsV3Query } from '$lib/features/library/queries/LocalV3Queries.svelte';
	import { formatBytes } from '$lib/utils/formatting';
	import { buildDiscoveryQueueFromLocal } from '$lib/features/player/playback/queueHelpers';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import { toastStore } from '$lib/stores/toast';
	import {
		fetchLibraryTracks,
		libraryTrackParams
	} from '$lib/features/library/queries/libraryTracks';
	import { createLibraryTrackQueue } from '$lib/features/library/queries/libraryTrackQueue.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import FritEdge from '$lib/ui/FritEdge.svelte';
	import Illustration from '$lib/ui/Illustration.svelte';
	import VinylRecord from '$lib/ui/VinylRecord.svelte';
	import { withBasePath } from '$lib/utils/basePath';

	const statsQuery = getLocalStatsV3Query();
	const stats = $derived(statsQuery.data);

	const queue = createLibraryTrackQueue({
		buildQueue: (tracks) => buildDiscoveryQueueFromLocal(tracks),
		playQueue: (items, startIndex, shuffle) => playerStore.playQueue(items, startIndex, shuffle),
		appendQueue: (items) => playerStore.appendQueueSilent(items),
		reshuffle: () => playerStore.regenerateShuffleOrder(),
		toast: (message, type) => toastStore.notify(message, type)
	});

	let busy = $state<'' | 'play' | 'shuffle' | 'surprise'>('');

	async function start(kind: 'play' | 'shuffle') {
		if (busy) return;
		busy = kind;
		try {
			const started =
				kind === 'play' ? await queue.playAll('recent', '') : await queue.shuffleAll('');
			if (started === 'empty') toastStore.notify('No tracks to play yet', 'info');
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
				toastStore.notify('Nothing to surprise you with yet', 'info');
				return;
			}
			playerStore.playQueue(buildDiscoveryQueueFromLocal([track]), 0, false);
		} catch {
			toastStore.notify("Couldn't pick a track", 'error');
		} finally {
			busy = '';
		}
	}
</script>

<section class="lf-band">
	<div class="lf-band__glow" aria-hidden="true"></div>
	<FritEdge class="lf-band__frit" />
	<VinylRecord label class="lf-band__record" />

	<!-- stretched link under the content so empty area navigates, while playback buttons (pointer-events re-enabled) still act in place; avoids nested interactives -->
	<a
		href={withBasePath('/library/local')}
		aria-label="Enter the Listening Room"
		class="lf-band__link"
	></a>

	<div class="lf-band__body">
		<div class="lf-band__head">
			<Illustration name="folder-sticker" class="lf-band__sticker" />
			<h2 class="lf-band__title">Local Files</h2>
			<span class="lf-band__rule" aria-hidden="true"></span>
			<p class="lf-band__counts">
				{#if stats}
					<span>{stats.total_tracks.toLocaleString()} tracks</span>
					<span>{stats.total_artists.toLocaleString()} artists</span>
					<span>{formatBytes(stats.total_size_bytes)}</span>
				{:else}
					<span>Your local music collection</span>
				{/if}
			</p>
		</div>

		<div class="lf-band__ticket-wrap" aria-hidden="true">
			<Card variant="torn" padding="none" class="lf-band__ticket">
				<span class="lf-band__enter">Enter the</span>
				<span class="lf-band__room">Listening Room<ArrowRight class="lf-band__arrow" /></span>
				<span class="lf-band__stripes dn-stripes"></span>
			</Card>
		</div>

		<div class="lf-band__buttons">
			<Button
				variant="primary"
				size="display"
				icon={Play}
				loading={busy === 'play'}
				class="pointer-events-auto"
				onclick={playAll}>Play All</Button
			>
			<Button
				variant="outline"
				size="display"
				icon={Shuffle}
				loading={busy === 'shuffle'}
				class="pointer-events-auto"
				onclick={shuffleAll}>Shuffle</Button
			>
			<Button
				variant="outline"
				size="display"
				icon={Sparkles}
				loading={busy === 'surprise'}
				class="pointer-events-auto"
				onclick={surprise}>Surprise me</Button
			>
		</div>
	</div>
</section>

<style>
	.lf-band {
		position: relative;
		isolation: isolate;
		overflow: hidden;
		min-height: 10rem;
		border: 1px solid color-mix(in oklab, var(--color-base-content) 30%, transparent);
		border-radius: 1rem;
		background-color: var(--color-base-100);
		transition: border-color 150ms;
	}
	.lf-band:hover {
		border-color: color-mix(in oklab, var(--color-base-content) 55%, transparent);
	}

	.lf-band__glow {
		position: absolute;
		inset: 0;
		z-index: -1;
		background:
			radial-gradient(
				ellipse 40% 120% at 0% 100%,
				color-mix(in oklab, var(--dn-title) 22%, transparent),
				transparent 70%
			),
			radial-gradient(
				ellipse 45% 140% at 62% 100%,
				color-mix(in oklab, var(--dn-title) 16%, transparent),
				transparent 70%
			);
	}
	.lf-band :global(.lf-band__frit) {
		z-index: -1;
	}

	.lf-band :global(.lf-band__record) {
		position: absolute;
		top: -2.25rem;
		left: 50%;
		z-index: -1;
		width: 22rem;
	}

	.lf-band__link {
		position: absolute;
		inset: 0;
		z-index: 10;
		border-radius: 1rem;
	}
	.lf-band__link:focus-visible {
		outline-offset: -4px;
	}

	.lf-band__body {
		position: relative;
		z-index: 20;
		display: grid;
		grid-template-columns: 1fr auto;
		grid-template-areas:
			'head ticket'
			'buttons ticket';
		align-items: center;
		gap: 0.25rem 1.5rem;
		padding: 0.875rem 1.25rem 0.875rem 0.875rem;
		pointer-events: none;
	}

	.lf-band__head {
		grid-area: head;
		display: flex;
		align-items: center;
		gap: 1.25rem;
	}
	.lf-band__head :global(.lf-band__sticker) {
		width: 5.25rem;
		height: 4.875rem;
		flex: none;
		rotate: -2deg;
	}
	.lf-band__title {
		font-family: var(--font-display);
		font-weight: 400;
		font-size: 3.875rem;
		line-height: 0.95;
		letter-spacing: 0.005em;
		text-transform: uppercase;
		white-space: nowrap;
		font-synthesis: none;
	}
	.lf-band__rule {
		width: 2px;
		height: 3.75rem;
		flex: none;
		background-color: color-mix(in oklab, var(--color-base-content) 60%, transparent);
	}
	.lf-band__counts {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 600;
		letter-spacing: 0.16em;
		text-transform: uppercase;
	}

	.lf-band__buttons {
		grid-area: buttons;
		display: flex;
		flex-wrap: wrap;
		gap: 0.875rem;
		padding-left: 4.5rem;
	}

	.lf-band__ticket-wrap {
		grid-area: ticket;
		filter: drop-shadow(0 8px 12px rgb(0 0 0 / 0.4));
		rotate: -1.5deg;
	}
	.lf-band__ticket-wrap :global(.lf-band__ticket) {
		width: 16rem;
		padding: 1.5rem 1.5rem 1.25rem 2rem;
	}
	.lf-band__enter {
		font-family: var(--font-mono);
		font-size: 0.875rem;
		font-weight: 600;
		letter-spacing: 0.3em;
		text-transform: uppercase;
	}
	.lf-band__room {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		margin-top: 0.25rem;
		font-family: var(--font-display);
		font-size: 1.75rem;
		line-height: 1;
		letter-spacing: 0.03em;
		text-transform: uppercase;
		white-space: nowrap;
		font-synthesis: none;
	}
	.lf-band__room :global(.lf-band__arrow) {
		width: 1.75rem;
		height: 1.75rem;
		flex: none;
	}
	.lf-band__stripes {
		display: block;
		height: 1.375rem;
		margin-top: 0.75rem;
		font-size: 1.25rem;
	}

	@media (max-width: 1023px) {
		.lf-band__body {
			grid-template-columns: 1fr;
			grid-template-areas:
				'head'
				'ticket'
				'buttons';
			gap: 0.875rem;
		}
		.lf-band__buttons {
			padding-left: 0;
		}
		.lf-band__ticket-wrap {
			rotate: none;
		}
		.lf-band__ticket-wrap :global(.lf-band__ticket) {
			display: flex;
			width: 100%;
			flex-direction: row;
			align-items: center;
			gap: 0.75rem;
			padding: 0.875rem 1.25rem;
		}
		.lf-band__ticket-wrap :global(.lf-band__ticket .card-body) {
			flex-direction: row;
			align-items: center;
			gap: 0.75rem;
		}
		.lf-band__room {
			margin-top: 0;
		}
		.lf-band__stripes {
			flex: 1;
			margin-top: 0;
			max-width: 8rem;
			margin-left: auto;
		}
	}

	/* Phone: the counts on one line under the title, the buttons share a row */
	@media (max-width: 639px) {
		.lf-band__body {
			padding: 0.875rem 0.75rem 0.875rem;
			gap: 0.75rem;
		}
		.lf-band__head {
			display: grid;
			grid-template-columns: auto 1fr;
			column-gap: 0.875rem;
		}
		.lf-band__head :global(.lf-band__sticker) {
			grid-row: 1 / 3;
			width: 3.5rem;
			height: 3.25rem;
		}
		.lf-band__title {
			font-size: 2.375rem;
		}
		.lf-band__rule {
			display: none;
		}
		.lf-band__counts {
			grid-column: 2;
			flex-direction: row;
			flex-wrap: wrap;
			gap: 0 0.375rem;
			font-size: 0.625rem;
			letter-spacing: 0.1em;
			white-space: nowrap;
		}
		.lf-band__counts span + span::before {
			content: '·';
			margin-right: 0.5rem;
		}
		/* the record keeps to the corner, above the counts and right of the title */
		.lf-band :global(.lf-band__record) {
			top: -4rem;
			left: auto;
			right: -3rem;
			width: 7rem;
		}
		.lf-band__enter {
			font-size: 0.75rem;
			letter-spacing: 0.2em;
			white-space: nowrap;
		}
		.lf-band__room {
			font-size: 1.125rem;
		}
		.lf-band__room :global(.lf-band__arrow) {
			width: 1.125rem;
			height: 1.125rem;
		}
		.lf-band__ticket-wrap :global(.lf-band__ticket) {
			padding: 0.625rem 0.875rem;
		}
		.lf-band__ticket-wrap :global(.lf-band__ticket .card-body) {
			gap: 0.5rem;
		}
		.lf-band__stripes {
			height: 1rem;
			font-size: 0.875rem;
		}
		.lf-band__buttons {
			display: grid;
			grid-template-columns: 1fr 1fr 1fr;
			gap: 0.5rem;
		}
		.lf-band__buttons :global(.btn) {
			--btn-p: 0.25rem;
			--fontsize: 0.8125rem;
			--size: 2.375rem;
			gap: 0.375rem;
			white-space: nowrap;
		}
		.lf-band__buttons :global(.btn svg) {
			width: 1rem;
			height: 1rem;
		}
	}

	/* too narrow for the record beside the title: the text keeps the room */
	@media (max-width: 359px) {
		.lf-band :global(.lf-band__record) {
			display: none;
		}
	}
</style>
