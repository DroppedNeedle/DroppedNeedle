<!--
	What happens after "Use this edition", live: the files being placed on
	it, then its tags and names, written by the app in a folder it manages
	by itself and left for the person in any other.
-->
<script lang="ts">
	import { CircleCheck, Undo2 } from 'lucide-svelte';
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import type {
		EditionChoice,
		EditionPlan,
		EditionStatus
	} from '$lib/queries/albums/EditionQueries.svelte';
	import { filesPlacedSentence, placingSentence, tagsSentence, type TagsMode } from './editionCopy';

	interface Props {
		name: string;
		status: EditionStatus | null;
		/** The album's own edition plan, once it is the chosen edition's. */
		plan: EditionPlan | null;
		choice: EditionChoice;
		/** The album is left unmanaged, by the person or by the app. */
		held: boolean;
		error: string | null;
		onundo: () => Promise<void>;
		/** Writes the edition's tags now, in a folder the app does not write by itself. */
		onwritetags?: () => Promise<void>;
		tagsWritten?: boolean;
		/** Asks for the edition's missing tracks. */
		ongetmissing?: () => Promise<void>;
		missingRequested?: string | null;
	}

	let {
		name,
		status,
		plan,
		choice,
		held,
		error,
		onundo,
		onwritetags,
		tagsWritten = false,
		ongetmissing,
		missingRequested = null
	}: Props = $props();

	const placing = $derived(status?.placing ?? null);
	const files = $derived(
		placing
			? placingSentence(placing.attempts)
			: plan
				? filesPlacedSentence(plan.kept_count, plan.files.length - plan.kept_count)
				: filesPlacedSentence(choice.retag_files.length, choice.extra_track_ids.length)
	);
	const tags = $derived.by((): TagsMode => {
		if (held) return 'held';
		if (status?.files_by_itself) return status.writing || placing ? 'writing' : 'written';
		return 'manual';
	});
</script>

<section class="flex flex-col gap-4" aria-labelledby="edition-next-steps-title">
	<div class="flex flex-wrap items-start justify-between gap-3">
		<h3 id="edition-next-steps-title" class="flex items-start gap-2 text-base font-semibold">
			<CircleCheck class="mt-0.5 h-5 w-5 shrink-0 text-success" aria-hidden="true" />
			<span>This is your edition now: {name}</span>
		</h3>
		<AsyncButton class="btn btn-ghost btn-sm gap-1" onclick={onundo}>
			<Undo2 class="h-4 w-4" aria-hidden="true" /> Undo this change
		</AsyncButton>
	</div>
	{#if error}
		<p class="text-sm text-error" role="alert">{error}</p>
	{/if}
	<dl class="grid grid-cols-[4rem_minmax(0,1fr)] gap-x-4 gap-y-3 text-sm" aria-live="polite">
		<dt class="font-semibold text-base-content/60">Files</dt>
		<dd class="flex items-start gap-2">
			{#if placing}<span class="loading loading-spinner loading-xs mt-0.5" aria-hidden="true"
				></span>{/if}
			<span>{files}</span>
		</dd>
		<dt class="font-semibold text-base-content/60">Tags</dt>
		<dd class="flex items-start gap-2">
			{#if tags === 'writing'}<span
					class="loading loading-spinner loading-xs mt-0.5"
					aria-hidden="true"
				></span>{/if}
			<span class="flex flex-col items-start gap-2">
				<span>{tagsWritten ? "The files now carry this edition's tags." : tagsSentence(tags)}</span>
				{#if tags === 'manual' && onwritetags && !tagsWritten && choice.retag_files.length}
					<AsyncButton
						class="btn btn-outline btn-sm"
						disabled={Boolean(placing)}
						onclick={onwritetags}>Write this edition's tags</AsyncButton
					>
				{/if}
			</span>
		</dd>
		{#if ongetmissing && choice.missing_titles.length}
			<dt class="font-semibold text-base-content/60">Tracks</dt>
			<dd class="flex flex-col items-start gap-2">
				{#if missingRequested}
					<span>{missingRequested}</span>
				{:else}
					<AsyncButton class="btn btn-outline btn-sm" onclick={ongetmissing}
						>Get the missing tracks</AsyncButton
					>
				{/if}
			</dd>
		{/if}
	</dl>
</section>
