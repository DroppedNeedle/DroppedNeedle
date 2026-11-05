<!--
	One gig, drawn as a ticket: a date stub (weekday, day, month) torn off
	along a perforation, the artist's picture and name, the venue line, and
	"Tickets" in acid when the listing has a link. A cancelled gig strikes
	its venue line through.
-->
<script lang="ts">
	import { ExternalLink } from 'lucide-svelte';
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import Button from '$lib/ui/Button.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import { outsideLink } from '$lib/ui/outsideLink';
	import type { Concert } from '$lib/features/following/queries/types';
	import { KM_PER_MILE } from '$lib/constants';
	import { withBasePath } from '$lib/utils/basePath';

	interface Props {
		concert: Concert;
	}

	let { concert }: Props = $props();

	function parseLocalDate(value: string): Date {
		const [year, month, day] = value.split('-').map(Number);
		return new Date(year, (month || 1) - 1, day || 1);
	}

	const date = $derived(parseLocalDate(concert.local_date));
	const weekday = $derived(date.toLocaleDateString(undefined, { weekday: 'short' }));
	const month = $derived(date.toLocaleDateString(undefined, { month: 'short' }));
	const artistHref = $derived(withBasePath(`/artist/${concert.artist_mbid}`));
	const ticketHref = $derived(outsideLink(concert.ticket_url));
	const cancelled = $derived(concert.status === 'cancelled');
	const venueLine = $derived(
		[
			concert.venue_name,
			concert.city,
			concert.distance_km != null ? `${Math.round(concert.distance_km / KM_PER_MILE)} mi` : null
		]
			.filter(Boolean)
			.join(' · ')
	);
</script>

<article class="dn-gig" class:dn-gig--cancelled={cancelled}>
	<div class="dn-gig__stub" aria-hidden="true">
		<span class="dn-gig__weekday">{weekday}</span>
		<span class="dn-gig__day">{date.getDate()}</span>
		<span class="dn-gig__month">{month}</span>
	</div>
	<span class="sr-only">{date.toLocaleDateString(undefined, { dateStyle: 'full' })}</span>

	<div class="dn-gig__body">
		<a href={artistHref} aria-label="Open {concert.artist_name}" class="dn-gig__avatar">
			<ArtistImage
				mbid={concert.artist_mbid}
				alt={concert.artist_name}
				size="full"
				rounded="full"
				className="h-full w-full"
			/>
		</a>

		<div class="dn-gig__text">
			<div class="dn-gig__head">
				<a href={artistHref} class="dn-gig__artist">{concert.artist_name}</a>
				{#if cancelled}
					<StatusChip tone="error" size="xs">Cancelled</StatusChip>
				{:else if concert.status === 'rescheduled'}
					<StatusChip tone="warning" size="xs">Rescheduled</StatusChip>
				{/if}
			</div>
			{#if concert.event_name && concert.event_name !== concert.artist_name}
				<p class="dn-gig__event">{concert.event_name}</p>
			{/if}
			<p class="dn-gig__venue" class:line-through={cancelled}>{venueLine}</p>
		</div>

		{#if ticketHref}
			<Button
				variant="primary"
				size="display-sm"
				icon={ExternalLink}
				iconPosition="end"
				href={ticketHref}
				target="_blank"
				rel="noopener noreferrer"
				class="dn-gig__tickets"
			>
				Tickets
			</Button>
		{/if}
	</div>
</article>

<style>
	/* the stub's width sets where the perforation and its notches fall */
	.dn-gig {
		--stub: 6.25rem;
		--notch: 0.625rem;
		position: relative;
		display: flex;
		min-height: 6.5rem;
		border: 1px solid var(--dn-line);
		border-radius: 0.625rem;
		background-color: var(--color-base-200);
		mask:
			radial-gradient(
					circle at var(--stub) 0,
					transparent var(--notch),
					#000 calc(var(--notch) + 0.5px)
				)
				top / 100% 51% no-repeat,
			radial-gradient(
					circle at var(--stub) 100%,
					transparent var(--notch),
					#000 calc(var(--notch) + 0.5px)
				)
				bottom / 100% 51% no-repeat;
		transition: border-color 150ms;
	}
	.dn-gig:hover {
		border-color: var(--dn-edge);
	}

	.dn-gig__stub {
		display: flex;
		width: var(--stub);
		flex: none;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		padding-block: 0.75rem;
		border-right: 2px dashed var(--dn-line);
		line-height: 1;
	}
	.dn-gig__weekday,
	.dn-gig__month {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 600;
		letter-spacing: 0.2em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.dn-gig__day {
		margin-block: 0.25rem 0.3rem;
		font-family: var(--font-display);
		font-size: 2.75rem;
		line-height: 0.95;
		color: var(--dn-title);
		font-synthesis: none;
	}
	.dn-gig--cancelled .dn-gig__day {
		color: var(--dn-muted);
	}

	.dn-gig__body {
		display: flex;
		min-width: 0;
		flex: 1;
		align-items: center;
		gap: 1.125rem;
		padding: 0.875rem 1.25rem 0.875rem 1.375rem;
	}
	.dn-gig__avatar {
		width: 3.5rem;
		height: 3.5rem;
		flex: none;
		border-radius: 9999px;
	}
	.dn-gig__text {
		min-width: 0;
		flex: 1;
	}
	.dn-gig__head {
		display: flex;
		min-width: 0;
		align-items: center;
		gap: 0.625rem;
	}
	.dn-gig__artist {
		overflow: hidden;
		font-size: 1.125rem;
		font-weight: 700;
		text-overflow: ellipsis;
		text-transform: uppercase;
		letter-spacing: 0.02em;
		white-space: nowrap;
	}
	.dn-gig__artist:hover {
		color: var(--dn-title);
		text-decoration: underline;
		text-underline-offset: 0.2em;
	}
	.dn-gig__event {
		margin-top: 0.125rem;
		overflow: hidden;
		font-size: 0.9375rem;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.dn-gig__venue {
		margin-top: 0.25rem;
		overflow: hidden;
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 500;
		letter-spacing: 0.08em;
		text-overflow: ellipsis;
		text-transform: uppercase;
		white-space: nowrap;
		color: var(--dn-muted);
	}
	.dn-gig__body :global(.dn-gig__tickets) {
		flex: none;
	}

	/* phone: the stub stays left; the picture shrinks beside the name and
	   Tickets drops under the venue line */
	@media (max-width: 639px) {
		.dn-gig {
			--stub: 4.5rem;
			--notch: 0.5rem;
		}
		.dn-gig__day {
			font-size: 2.25rem;
		}
		.dn-gig__weekday,
		.dn-gig__month {
			font-size: 0.6875rem;
			letter-spacing: 0.14em;
		}
		.dn-gig__body {
			display: grid;
			grid-template-columns: auto minmax(0, 1fr);
			align-items: center;
			gap: 0.5rem 0.75rem;
			padding: 0.75rem 0.875rem 0.75rem 0.875rem;
		}
		.dn-gig__avatar {
			width: 2.5rem;
			height: 2.5rem;
			align-self: start;
		}
		.dn-gig__artist {
			font-size: 1rem;
		}
		.dn-gig__head {
			flex-wrap: wrap;
			gap: 0.25rem 0.5rem;
		}
		.dn-gig__venue {
			font-size: 0.75rem;
			letter-spacing: 0.04em;
			white-space: normal;
		}
		.dn-gig__body :global(.dn-gig__tickets) {
			--size: 2rem;
			--btn-p: 0.75rem;
			--fontsize: 0.9375rem;
			grid-column: 2;
			justify-self: start;
		}
	}

	@media (prefers-contrast: more), (forced-colors: active) {
		.dn-gig {
			mask: none;
		}
	}
</style>
