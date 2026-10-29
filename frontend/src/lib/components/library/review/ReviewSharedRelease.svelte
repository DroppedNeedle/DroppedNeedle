<!--
	Use one release for every selected review: lists the releases they all
	list (from the bulk preview), and applies the chosen one with a fresh
	preview of exactly that change.
-->
<script lang="ts">
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import {
		applyBulkLibraryReviews,
		previewBulkLibraryReviews,
		type BulkReviewPreview,
		type BulkReviewResult,
		type BulkReviewSelection
	} from '$lib/queries/library/LibraryReviewMutations.svelte';

	interface Props {
		open: boolean;
		/** The selection as an accept without a release. */
		selection: BulkReviewSelection | null;
		/** The preview of that selection, which lists the shared releases. */
		plan: BulkReviewPreview | null;
		ondone: (result: BulkReviewResult) => void;
	}

	let { open = $bindable(false), selection, plan, ondone }: Props = $props();

	const preview = previewBulkLibraryReviews();
	const apply = applyBulkLibraryReviews();
	const shared = $derived(plan?.shared_candidates ?? []);
	// The best shared release until the person picks another.
	let chosen = $derived<string | null>(shared[0]?.candidate_key ?? null);

	function percent(score: number): string {
		return `${Math.round(Math.max(0, Math.min(1, score)) * 100)}%`;
	}

	async function useChosen(): Promise<void> {
		if (!selection || !chosen) return;
		const named = { ...selection, action: 'accept' as const, candidateKey: chosen };
		const exact = await preview.mutateAsync(named);
		ondone(await apply.mutateAsync({ selection: named, previewToken: exact.preview_token }));
	}
</script>

<ConfirmDialog
	bind:open
	title="Use a shared release"
	confirmLabel="Use this release"
	canConfirm={Boolean(chosen)}
	onconfirm={useChosen}
>
	{#if shared.length}
		<fieldset class="space-y-2">
			<legend class="mb-2 text-sm">Every selected album could be one of these.</legend>
			{#each shared as candidate (candidate.candidate_key)}
				<label
					class="flex cursor-pointer items-center gap-3 rounded-lg bg-base-200 px-3 py-2 text-sm"
				>
					<input
						type="radio"
						class="radio radio-sm"
						name="shared-release"
						value={candidate.candidate_key}
						bind:group={chosen}
					/>
					<span class="min-w-0">
						<span class="block truncate font-medium">{candidate.album_title}</span>
						<span class="block truncate text-xs text-base-content/60"
							>{candidate.album_artist_name} · {percent(candidate.lowest_score)} fit or better</span
						>
					</span>
				</label>
			{/each}
			<p class="text-xs text-base-content/60">
				Each album is pinned to it. Undo each from its album page.
			</p>
		</fieldset>
	{:else}
		<p class="text-sm">These albums share no release. Pick one for each instead.</p>
	{/if}
</ConfirmDialog>
