<!--
	Discover's slim header: the title, the week's ticket, two mono lines
	saying how many albums and when the next set comes, and the caps links.
	While a service is not answering, an amber tape takes the lines' place.
	On a phone the title and the week's spines, which jump to the bin, share
	the first row, with where the person is in the week on one line under it.
-->
<script lang="ts">
	import Button from '$lib/ui/Button.svelte';
	import BarcodeTicket from '$lib/ui/BarcodeTicket.svelte';
	import Tape from '$lib/ui/Tape.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import type { WeekResponse } from '$lib/features/discover/queries/week.svelte';
	import { clockTime, dayDate, longDayDate } from '$lib/features/discover/state/weekWords';
	import WeekSpines, { type Spine } from './WeekSpines.svelte';

	interface Props {
		week: WeekResponse | null;
		/** Albums on the Later pile; the link shows only while it holds some. */
		laterCount: number;
		/** "4 of 8" on a phone, or "Extra 2"; none while nothing is on the counter. */
		position: string | null;
		spines: Spine[];
		current: string | null;
		onjump: () => void;
		ontold: () => void;
	}

	let { week, laterCount, position, spines, current, onjump, ontold }: Props = $props();

	const SOURCES = { listenbrainz: 'ListenBrainz', musicbrainz: 'MusicBrainz', lastfm: 'Last.fm' };

	const status = $derived(week?.status ?? null);
	const set = $derived(
		status === 'ready' || status === 'kept_last_week' ? (week?.set ?? null) : null
	);
	const failing = $derived(status === 'kept_last_week' ? (week?.failing ?? null) : null);
	const lines = $derived.by((): [string, string] | null => {
		if (status === 'being_set_out')
			return ['Being set out now', `${week?.found_so_far ?? 0} of 8 found`];
		if (!set) return null;
		return [
			`${set.picks.length} ${set.picks.length === 1 ? 'album' : 'albums'} for you`,
			`Next set ${longDayDate(set.next_set_at)}`
		];
	});
	const laterHref = withBasePath('/discover/later');
</script>

{#snippet links()}
	{#if laterCount > 0}
		<Button variant="caps" size="sm" href={laterHref}>Later ({laterCount})</Button>
	{/if}
	<Button variant="caps" size="sm" onclick={ontold}>What you told Discover</Button>
{/snippet}

<header class="dn-week-head">
	<h1 class="dn-week-head__title">Discover</h1>
	{#if set}
		<BarcodeTicket
			figure={String(set.week_number)}
			lines={['Week', `Set out ${dayDate(set.week_start_at)}`]}
			spoken="Week {set.week_number}, set out {dayDate(set.week_start_at)}"
			class="dn-week-head__ticket"
		/>
	{/if}
	{#if failing && set}
		<div class="dn-week-head__notice" role="status">
			<Tape look="warn">
				{SOURCES[failing.source]} has not answered since {clockTime(failing.since)}
			</Tape>
			<span>
				This is still week {set.week_number}'s set. Week {set.week_number + 1} is set out as soon as it
				answers.
			</span>
		</div>
	{:else if lines}
		<p class="dn-week-head__lines">
			<span>{lines[0]}</span>
			<span>{lines[1]}</span>
		</p>
	{/if}
	<nav class="dn-week-head__links" aria-label="Discover">{@render links()}</nav>

	<div class="dn-week-head__phone">
		{#if set && position}
			<p class="dn-week-head__where">
				Week {set.week_number} - {position}<span>{` - Next ${dayDate(set.next_set_at)}`}</span>
			</p>
		{/if}
		{#if spines.length}
			<WeekSpines {spines} {current} {onjump} />
		{/if}
	</div>
</header>

<style>
	.dn-week-head {
		display: flex;
		align-items: center;
		gap: 1.5rem;
		min-height: 3.75rem;
		margin-bottom: 0.75rem;
	}
	.dn-week-head__title {
		margin: 0;
		font-family: var(--font-display);
		font-size: 3.5rem;
		font-weight: 400;
		line-height: 0.9;
		text-transform: uppercase;
		color: var(--dn-title);
		font-synthesis: none;
	}
	.dn-week-head :global(.dn-week-head__ticket) {
		width: 15.5rem;
		rotate: -2.5deg;
	}
	.dn-week-head__lines {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		margin: 0;
		font-family: var(--font-mono);
		font-size: 0.78125rem;
		font-weight: 500;
		line-height: 1;
		letter-spacing: 0.16em;
		text-transform: uppercase;
	}
	.dn-week-head__lines span + span {
		color: var(--dn-muted);
	}
	.dn-week-head__notice {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 0.25rem;
		font-size: 0.84375rem;
	}
	.dn-week-head__notice > span {
		color: var(--dn-muted);
	}
	.dn-week-head__links {
		display: flex;
		gap: 0.25rem;
		margin-left: auto;
	}
	.dn-week-head__phone {
		display: none;
	}

	@media (max-width: 639px) {
		.dn-week-head {
			gap: 0.625rem;
			min-height: 3.125rem;
			margin-bottom: 0.5rem;
		}
		.dn-week-head__title {
			font-size: 1.875rem;
		}
		.dn-week-head :global(.dn-week-head__ticket),
		.dn-week-head__lines,
		.dn-week-head__links {
			display: none;
		}
		.dn-week-head {
			flex-wrap: wrap;
			row-gap: 0.25rem;
		}
		.dn-week-head__notice {
			order: 3;
			flex-basis: 100%;
		}
		/* its two parts join the header's rows: the spines beside the title,
		   the line under both */
		.dn-week-head__phone {
			display: contents;
		}
		.dn-week-head__where {
			order: 4;
			flex-basis: 100%;
			margin: 0;
			font-family: var(--font-mono);
			font-size: 0.65625rem;
			font-weight: 600;
			line-height: 1.35;
			letter-spacing: 0.12em;
			text-transform: uppercase;
			white-space: nowrap;
		}
		.dn-week-head__where span {
			color: var(--dn-muted);
		}
	}
</style>
