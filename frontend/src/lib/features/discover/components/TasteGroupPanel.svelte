<!--
	One taste group, open under its divider tab: its name, share of each
	set, trend and last play, the buttons for the whole group, then its
	artists, most played first, each with its plays, what was said about it
	and its own buttons. A statement about the whole group is pressed on the
	group's buttons while one of its artists carries it.
-->
<script lang="ts">
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import Button from '$lib/ui/Button.svelte';
	import type { TasteGroup, TasteStatement } from '$lib/features/discover/queries/taste.svelte';
	import {
		aroundName,
		isArtistTuning,
		TUNING_WORDS,
		type ArtistTuning
	} from '$lib/features/discover/state/tuningWords';
	import { dayMonth } from '$lib/features/discover/state/weekWords';
	import TasteButtons from './TasteButtons.svelte';
	import TasteShare from './TasteShare.svelte';

	interface Props {
		group: TasteGroup;
		busy: boolean;
		onsay: (statement: TasteStatement) => void;
		onundo: (undo: { kind: ArtistTuning; target: string; group: boolean }) => void;
	}

	let { group, busy, onsay, onundo }: Props = $props();

	const name = $derived(aroundName(group.anchor_name));
	const said = (artist: TasteGroup['artists'][number]): ArtistTuning | null =>
		artist.tuning && isArtistTuning(artist.tuning.kind) ? artist.tuning.kind : null;

	// The group's own statement: one of its artists carries a row said for it.
	const groupSaid = $derived.by(() => {
		for (const artist of group.artists) {
			const kind = said(artist);
			if (kind && artist.tuning?.group_anchor_mbid === group.anchor_artist_mbid)
				return { kind, target: artist.artist_mbid };
		}
		return null;
	});

	const count = (n: number) => n.toLocaleString();
	function plays(artist: TasteGroup['artists'][number]): string {
		const own = `${count(artist.plays)} ${artist.plays === 1 ? 'play' : 'plays'}`;
		return artist.imported_listens > 0
			? `${own} - ${count(artist.imported_listens)} imported`
			: own;
	}
</script>

<div class="dn-taste-group">
	<div class="dn-taste-group__head">
		<div class="dn-taste-group__title">
			<h2>{name}</h2>
			<p class="dn-taste-group__facts">
				<TasteShare
					share={group.share}
					trend={group.trend}
					words={(percent) => `${percent}% of each set`}
					wide
				/>
				{#if group.last_played_at}
					<span class="dn-taste-group__last">
						Last played {dayMonth(group.last_played_at * 1000)}
					</span>
				{/if}
			</p>
		</div>
		<div class="dn-taste-group__whole">
			<span class="dn-taste-group__caps">The whole group</span>
			<TasteButtons
				label="The whole group"
				current={groupSaid?.kind ?? null}
				{busy}
				onchoose={(kind) => onsay({ kind, group_no: group.group_no })}
				onundo={(kind) => groupSaid && onundo({ kind, target: groupSaid.target, group: true })}
			/>
		</div>
	</div>

	<ul class="dn-taste-group__artists">
		{#each group.artists as artist (artist.artist_mbid)}
			{@const kind = said(artist)}
			<li class="dn-taste-artist">
				<ArtistImage
					mbid={artist.artist_mbid}
					alt=""
					size="sm"
					rounded="full"
					className="shrink-0"
				/>
				<div class="dn-taste-artist__what">
					<p class="dn-taste-artist__name">{artist.name}</p>
					<p class="dn-taste-artist__plays">{plays(artist)}</p>
				</div>
				<p class="dn-taste-artist__state">{kind ? TUNING_WORDS[kind].state : ''}</p>
				<TasteButtons
					label={artist.name}
					current={kind}
					{busy}
					onchoose={(chosen) => onsay({ kind: chosen, target: artist.artist_mbid })}
					onundo={(undone) => onundo({ kind: undone, target: artist.artist_mbid, group: false })}
				/>
				<div class="dn-taste-artist__undo">
					{#if kind}
						<Button
							variant="caps-link"
							size="xs"
							aria-label="Undo for {artist.name}"
							disabled={busy}
							onclick={() => onundo({ kind, target: artist.artist_mbid, group: false })}
						>
							Undo
						</Button>
					{/if}
				</div>
			</li>
		{/each}
	</ul>
</div>

<style>
	.dn-taste-group {
		padding: 1.25rem 1.5rem 0.875rem;
	}
	.dn-taste-group__head {
		display: flex;
		flex-wrap: wrap;
		align-items: flex-end;
		gap: 1rem 1.5rem;
		padding-bottom: 0.875rem;
	}
	.dn-taste-group__title h2 {
		margin: 0;
		font-family: var(--font-display);
		font-size: 2.25rem;
		font-weight: 400;
		line-height: 1;
		text-transform: uppercase;
	}
	.dn-taste-group__facts {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.375rem 0.75rem;
		margin: 0.625rem 0 0;
	}
	.dn-taste-group__last,
	.dn-taste-group__caps {
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		opacity: 0.8;
	}
	.dn-taste-group__whole {
		display: flex;
		flex-direction: column;
		align-items: flex-end;
		gap: 0.375rem;
		margin-left: auto;
	}
	.dn-taste-group__artists {
		margin: 0;
		padding: 0;
		list-style: none;
	}
	.dn-taste-artist {
		display: grid;
		grid-template-columns: auto minmax(8rem, 13rem) minmax(0, 1fr) auto 4rem;
		align-items: center;
		gap: 0.875rem;
		padding: 0.625rem 0;
		border-top: 1px solid var(--dn-paper-border);
	}
	.dn-taste-artist__what {
		min-width: 0;
	}
	.dn-taste-artist__name {
		margin: 0;
		overflow: hidden;
		font-weight: 600;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.dn-taste-artist__plays,
	.dn-taste-artist__state {
		margin: 0;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		letter-spacing: 0.06em;
		opacity: 0.8;
	}
	.dn-taste-artist__plays {
		text-transform: uppercase;
	}
	.dn-taste-artist__undo {
		display: flex;
		justify-content: flex-end;
	}

	@media (max-width: 639px) {
		.dn-taste-group {
			padding: 0.875rem 0.875rem 0.25rem;
		}
		.dn-taste-group__title h2 {
			font-size: 1.625rem;
		}
		.dn-taste-group__whole {
			flex-direction: row;
			flex-wrap: wrap;
			align-items: center;
			margin-left: 0;
		}
		.dn-taste-group__caps {
			width: 3.5rem;
			font-size: 0.5625rem;
		}
		.dn-taste-artist {
			grid-template-columns: auto minmax(0, 1fr) auto;
			gap: 0.375rem 0.625rem;
		}
		.dn-taste-artist__state {
			grid-column: 2 / -1;
			grid-row: 2;
		}
		.dn-taste-artist__state:empty {
			display: none;
		}
		.dn-taste-artist :global(.dn-taste-buttons) {
			grid-column: 1 / -1;
		}
		.dn-taste-artist__undo {
			grid-column: 3;
			grid-row: 1;
		}
	}
</style>
