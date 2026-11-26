<!--
	One quiet line saying how a Discover section stands when it is not
	plainly built: kept from an earlier build, its source down, waiting for a
	linked account, thin, empty or still building. The counter uses it for
	a week with nothing set out yet.
-->
<script lang="ts">
	import Button from '$lib/ui/Button.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { PROVIDER_NAMES } from '$lib/utils/serviceError';
	import type { SectionKey, SectionState } from '../queries/types';

	interface Props {
		status: Pick<SectionState, 'state'> & Partial<Pick<SectionState, 'key' | 'source' | 'since'>>;
		/** The section's name, for a line that sits among other sections. */
		title?: string;
	}

	let { status, title }: Props = $props();

	// sections filled from the library or the person's own plays, which
	// following artists does not fill
	const OWN_SHELVES: ReadonlySet<SectionKey> = new Set([
		'anniversaries',
		'gaps_in_your_artists',
		'genre_list',
		'from_your_shelves',
		'lastfm_recent_scrobbles',
		'lastfm_weekly_artist_chart',
		'lastfm_weekly_album_chart'
	]);
	const followable = $derived(!status.key || !OWN_SHELVES.has(status.key));

	const service = $derived(status.source ? PROVIDER_NAMES[status.source] : null);
	const down = $derived(`${service ?? 'A music service'} isn't answering.`);
	// a kept section is under a week old, so the weekday and time name it
	const when = $derived(
		status.since
			? new Date(status.since * 1000).toLocaleString(undefined, {
					weekday: 'short',
					hour: 'numeric',
					minute: '2-digit'
				})
			: 'earlier'
	);

	const text = $derived.by(() => {
		switch (status.state) {
			case 'kept':
				return `${down} Showing these from ${when}.`;
			case 'source_down':
				return `${down} This fills in when it's back.`;
			case 'needs_link':
				return `Connect ${service ?? 'a listening service'} to fill this.`;
			case 'thin':
				return "That's all for now.";
			case 'empty':
				return followable
					? 'Nothing here yet. Play, heart or follow a few artists.'
					: 'Nothing here yet.';
			case 'building':
				return 'Still building.';
		}
	});
</script>

<p class="dn-state-line" role="status">
	{#if title}<span class="dn-state-line__title">{title}</span>{/if}
	<span>{text}</span>
	{#if status.state === 'empty' && followable}
		<Button variant="caps" size="xs" href={withBasePath('/search')}>Follow artists</Button>
	{:else if status.state === 'needs_link' && service}
		<Button variant="caps" size="xs" href={withBasePath('/profile#scrobbling')}>
			Connect {service}
		</Button>
	{/if}
</p>

<style>
	.dn-state-line {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.25rem 0.75rem;
		font-size: 0.875rem;
		color: var(--dn-muted);
	}
	.dn-state-line__title {
		font-weight: 600;
		color: var(--dn-title);
	}
</style>
