<!--
	One identification review: the album, why it waits, and when opened what
	it rests on and the actions the caller may take.
-->
<script lang="ts">
	import { ChevronDown, ChevronRight } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import type { LibraryReview } from '$lib/queries/library/LibraryReviewQueries.svelte';
	import type { ReviewAction } from '$lib/queries/library/LibraryReviewMutations.svelte';
	import ReviewEvidence from './ReviewEvidence.svelte';

	interface Props {
		review: LibraryReview;
		open: boolean;
		/** Shows the selection box. */
		selectable: boolean;
		selected: boolean;
		/** Every match is selected, so single boxes are locked on. */
		lockedSelected: boolean;
		busy: boolean;
		ontoggleopen: () => void;
		ontoggleselect: () => void;
		onaction: (action: ReviewAction) => void;
		onchoose: (candidateKey: string) => void;
		onretry: () => void;
	}

	let {
		review,
		open,
		selectable,
		selected,
		lockedSelected,
		busy,
		ontoggleopen,
		ontoggleselect,
		onaction,
		onchoose,
		onretry
	}: Props = $props();

	const STATE_LABEL: Record<LibraryReview['state'], string> = {
		pending: 'Needs a decision',
		approved: 'Resolved',
		dismissed: 'Closed',
		rejected: 'Kept as tagged',
		excluded: 'Excluded'
	};
</script>

<li class="rounded-box border border-base-content/10 bg-base-100">
	<div class="flex items-start gap-3 p-3">
		{#if selectable}
			<input
				type="checkbox"
				class="checkbox checkbox-sm mt-0.5"
				aria-label={`Select ${review.album_title}`}
				checked={lockedSelected || selected}
				disabled={lockedSelected}
				onchange={ontoggleselect}
			/>
		{/if}
		<button
			class="flex min-w-0 flex-1 items-start gap-2 text-left"
			aria-expanded={open}
			onclick={ontoggleopen}
		>
			{#if open}<ChevronDown class="mt-0.5 h-4 w-4 shrink-0" />{:else}<ChevronRight
					class="mt-0.5 h-4 w-4 shrink-0"
				/>{/if}
			<span class="min-w-0">
				<span class="block truncate font-medium">{review.album_title || 'Unknown album'}</span>
				<span class="block truncate text-xs text-base-content/55">{review.artist_name}</span>
				<span class="mt-1 block text-sm text-base-content/70">{review.reason.message}</span>
				<span class="block text-xs text-base-content/55">{review.reason.action}</span>
			</span>
		</button>
		<div class="flex shrink-0 flex-col items-end gap-2 sm:flex-row sm:items-center">
			{#if review.state !== 'pending'}
				<span class="badge badge-ghost badge-sm whitespace-nowrap">{STATE_LABEL[review.state]}</span
				>
			{/if}
			<Button
				variant="ghost"
				size="sm"
				href={withBasePath(`/album/${encodeURIComponent(review.album_id)}`)}>Open album</Button
			>
		</div>
	</div>

	{#if open}
		<div class="space-y-3 border-t border-base-content/10 p-3">
			<ReviewEvidence {review} {busy} {onchoose} />

			<div class="flex flex-wrap gap-2">
				{#if review.state === 'pending'}
					{#if authStore.isTrusted}
						<Button size="sm" disabled={busy} onclick={() => onaction('keep_tagged')}
							>Keep as tagged</Button
						>
						<Button
							variant="ghost"
							size="sm"
							disabled={busy}
							title="Take the MusicBrainz match off; the album keeps its own tags"
							onclick={() => onaction('detach')}>Remove match</Button
						>
						<Button variant="ghost" size="sm" disabled={busy} onclick={() => onaction('dismiss')}
							>Close without deciding</Button
						>
					{/if}
					{#if authStore.isAdmin}
						<!-- Retrying would only meet the edition a person chose. -->
						{#if !review.manual_identity}
							<Button variant="ghost" size="sm" disabled={busy} onclick={onretry}
								>Retry identification</Button
							>
						{/if}
						<Button
							variant="danger-ghost"
							size="sm"
							disabled={busy}
							onclick={() => onaction('exclude')}>Take out of library</Button
						>
					{/if}
				{:else if review.state === 'excluded' && authStore.isAdmin}
					<Button size="sm" disabled={busy} onclick={() => onaction('restore')}
						>Put back in library</Button
					>
				{/if}
			</div>
		</div>
	{/if}
</li>
