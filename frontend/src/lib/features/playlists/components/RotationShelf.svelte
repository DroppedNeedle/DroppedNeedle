<!--
	"On rotation" at the top of Playlists: the lists the app makes from the
	person's library, when the next change is, and a dashed tile naming the
	ideas waiting their turn and those skipped for having too few songs.
	Shows nothing while lists on rotation are off or none is live.
-->
<script lang="ts">
	import type { PlaylistSummary } from '$lib/api/playlists';
	import { authStore } from '$lib/stores/authStore.svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';
	import { getRotationQuery } from '$lib/features/playlists/queries/rotation.svelte';
	import PlaylistCard from './PlaylistCard.svelte';
	import { changeDay, ideaName } from './rotationIdeas';

	interface Props {
		/** The caller's lists on rotation. */
		lists: PlaylistSummary[];
		ondelete?: (playlistId: string) => void;
	}

	let { lists, ondelete }: Props = $props();

	const rotationQuery = getRotationQuery(() => authStore.isAuthenticated);

	const status = $derived(rotationQuery.data);
	// The lists show while their status loads or fails; only "off" hides them.
	const shown = $derived(lists.length > 0 && status?.enabled !== false);
	const nextChange = $derived(
		status?.next_change_at ?? lists.find((list) => list.rotation)?.rotation?.next_change_at
	);
	const waiting = $derived((status?.waiting ?? []).map(ideaName).join(' · '));
	const thin = $derived((status?.thin ?? []).map(ideaName).join(', '));
</script>

{#if shown}
	<section aria-label="On rotation">
		<SectionHead title="On rotation">
			{#snippet chips()}
				<span class="rot-when"
					>From your library{nextChange ? ` · next change ${changeDay(nextChange)}` : ''}</span
				>
			{/snippet}
		</SectionHead>
		<div class="rot-grid">
			{#each lists as playlist (playlist.id)}
				<PlaylistCard {playlist} {ondelete} />
			{/each}
			{#if waiting || thin}
				<div class="rot-waiting">
					{#if waiting}
						<p class="rot-waiting__head">Waiting their turn</p>
						<p class="rot-waiting__ideas">{waiting}</p>
					{/if}
					{#if thin}
						<p class="rot-waiting__thin">Skipped while thin: {thin}</p>
					{/if}
				</div>
			{/if}
		</div>
	</section>
{/if}

<style>
	.rot-when {
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}

	/* the Playlists page's grid: one card across a phone, two on a desktop */
	.rot-grid {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: 1.25rem;
	}
	@media (min-width: 900px) {
		.rot-grid {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}
	}
	@media (min-width: 1800px) {
		.rot-grid {
			grid-template-columns: repeat(3, minmax(0, 1fr));
		}
	}

	.rot-waiting {
		display: flex;
		flex-direction: column;
		justify-content: center;
		gap: 0.5rem;
		min-height: 7.75rem;
		padding: 1rem 1.25rem;
		border: 1px dashed var(--dn-line);
		border-radius: var(--radius-box);
	}
	.rot-waiting__head {
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.12em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.rot-waiting__ideas {
		font-size: 0.875rem;
		line-height: 1.6;
	}
	.rot-waiting__thin {
		font-size: 0.8125rem;
		color: var(--dn-muted);
	}
</style>
