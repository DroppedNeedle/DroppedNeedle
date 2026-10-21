<!--
	An album's removals whose files are still in the recycle bin, each with
	"Put back". Curators only; nothing shows while there is none.
-->
<script lang="ts">
	import { Trash2, Undo2 } from 'lucide-svelte';
	import { failureSentence } from '$lib/components/library/edition/editionCopy';
	import {
		getAlbumRemovalsQuery,
		putBackRemoval,
		type AlbumRemoval
	} from '$lib/queries/library/RemovalQueries.svelte';
	import { toastStore } from '$lib/stores/toast';
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import Card from '$lib/ui/Card.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import { removalLine, putBackToast, notBackLine } from './recycledRemovalsCopy';

	interface Props {
		/** The local album id. */
		albumId: string;
		/** The album's release group, so its page's caches refresh. */
		releaseGroupMbid?: string | null;
		/** After a put-back, for the page's own reads. */
		onputback?: () => void;
		/** Spacing around the card, when there is one. */
		class?: string;
	}

	let { albumId, releaseGroupMbid = null, onputback, class: className = '' }: Props = $props();

	const removalsQuery = getAlbumRemovalsQuery(() => albumId);
	const putBack = putBackRemoval();
	let notes = $state<Record<string, string>>({});
	let notBack = $state<string | null>(null);

	async function run(removal: AlbumRemoval): Promise<void> {
		notes = { ...notes, [removal.id]: '' };
		notBack = null;
		try {
			const answer = await putBack.mutateAsync({
				albumId,
				removalId: removal.id,
				releaseGroupMbid
			});
			toastStore.show({
				message: putBackToast(removal.files - answer.missing.length),
				type: 'success'
			});
			notBack = notBackLine(answer.missing);
			onputback?.();
		} catch (error) {
			notes = { ...notes, [removal.id]: failureSentence(error, "The files couldn't be put back.") };
		}
	}
</script>

<QueryBoundary query={removalsQuery} what="removed files">
	{#snippet loading()}{/snippet}
	{#snippet empty()}
		{#if notBack}<p class={['text-sm text-warning', className]} role="status">{notBack}</p>{/if}
	{/snippet}
	{#snippet failed()}{/snippet}
	{#snippet children(removals)}
		<Card
			as="section"
			variant="bordered"
			padding="md"
			class={className}
			aria-label="In the recycle bin"
		>
			<div class="flex items-start gap-3">
				<div
					class="grid size-9 shrink-0 place-items-center rounded-xl bg-base-200 text-base-content/70"
					aria-hidden="true"
				>
					<Trash2 class="h-5 w-5" />
				</div>
				<div class="min-w-0 flex-1">
					<p class="text-[0.66rem] font-extrabold tracking-[0.12em] text-base-content/55 uppercase">
						In the recycle bin
					</p>
					<ul class="mt-2 flex flex-col gap-3">
						{#each removals as removal (removal.id)}
							<li class="flex flex-wrap items-center justify-between gap-x-3 gap-y-2">
								<span class="text-sm">{removalLine(removal)}</span>
								{#if removal.held}
									<span class="text-xs text-base-content/60"
										>Waits for an admin on Library Management</span
									>
								{:else}
									<AsyncButton
										variant="outline"
										size="sm"
										icon={Undo2}
										pendingLabel="Putting back..."
										onclick={() => run(removal)}>Put back</AsyncButton
									>
								{/if}
								{#if notes[removal.id]}
									<p class="basis-full text-sm text-error" role="alert">{notes[removal.id]}</p>
								{/if}
							</li>
						{/each}
					</ul>
					{#if notBack}<p class="mt-3 text-sm text-warning" role="status">{notBack}</p>{/if}
				</div>
			</div>
		</Card>
	{/snippet}
</QueryBoundary>
