<!--
	Writing the chosen edition's tags in a folder DroppedNeedle does not
	write by itself: a preview of what changes in each file, then the write,
	then its undo. Nothing is written until "Write tags".
-->
<script lang="ts">
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import {
		previewEditionTags,
		undoTagWrite,
		writeEditionTags,
		type EditionChoice,
		type TagWrite,
		type TagWritePreview
	} from '$lib/queries/albums/EditionQueries.svelte';
	import {
		alreadyTaggedSentence,
		failureSentence,
		tagChangeLine,
		tagsSentence,
		writeTagsLabel
	} from './editionCopy';

	interface Props {
		album: { userId: string | null | undefined; localId: string; rgMbid?: string };
		files: EditionChoice['retag_files'];
		/** The files are still being placed on the edition: no preview yet. */
		placing: boolean;
		/** The id of the sentence that says why the preview waits. */
		placingSentenceId: string;
	}

	let { album, files, placing, placingSentenceId }: Props = $props();

	const preview = previewEditionTags();
	const write = writeEditionTags();
	const undo = undoTagWrite();

	let planned = $state<TagWritePreview | null>(null);
	let written = $state<TagWrite | null>(null);
	let undone = $state(false);
	let error = $state<string | null>(null);

	const changed = $derived(planned?.files.filter((file) => file.changes.length) ?? []);
	const unchanged = $derived((planned?.files.length ?? 0) - changed.length);

	function fileName(source: string): string {
		return source.slice(source.lastIndexOf('/') + 1);
	}

	async function showPreview(): Promise<void> {
		error = null;
		undone = false;
		try {
			planned = await preview.mutateAsync({ ...album, files });
		} catch (failure) {
			error = failureSentence(failure, "The new tags couldn't be previewed.");
		}
	}

	async function writeTags(): Promise<void> {
		const token = planned?.preview_token;
		if (!token) return;
		error = null;
		try {
			written = await write.mutateAsync({ ...album, previewToken: token });
		} catch (failure) {
			error = failureSentence(failure, "The tags couldn't be written.");
		}
		// A preview token is good for one write, so a refused one is shown again fresh.
		planned = null;
	}

	async function undoWrite(): Promise<void> {
		const bundleId = written?.bundle_id;
		if (!bundleId) return;
		error = null;
		try {
			await undo.mutateAsync({ ...album, bundleId });
			written = null;
			undone = true;
		} catch (failure) {
			error = failureSentence(failure, "The tags couldn't be put back.");
		}
	}
</script>

<div class="flex flex-col items-start gap-2">
	{#if written}
		<span class="flex flex-wrap items-center gap-2">
			<span>Tags written.</span>
			<AsyncButton class="btn btn-ghost btn-xs" onclick={undoWrite}>Undo</AsyncButton>
		</span>
	{:else}
		<span>{undone ? 'The old tags are back.' : tagsSentence('manual')}</span>
		{#if planned}
			{#if changed.length}
				<ul class="flex w-full flex-col gap-2" aria-label="Tags that change">
					{#each changed as file (file.track_id)}
						<li class="rounded-box bg-base-200 px-3 py-2">
							<span class="block font-medium break-all">{fileName(file.source)}</span>
							<ul class="text-base-content/70">
								{#each file.changes as change (change.field)}
									<li class="break-words">{tagChangeLine(change)}</li>
								{/each}
							</ul>
						</li>
					{/each}
				</ul>
			{/if}
			{#if unchanged}
				<span class="text-base-content/70">{alreadyTaggedSentence(unchanged)}</span>
			{/if}
			{#if changed.length}
				<AsyncButton class="btn btn-primary btn-sm" onclick={writeTags}
					>{writeTagsLabel(changed.length)}</AsyncButton
				>
			{/if}
		{:else}
			<AsyncButton
				class="btn btn-outline btn-sm"
				disabled={placing}
				aria-describedby={placing ? placingSentenceId : undefined}
				onclick={showPreview}>Preview the new tags</AsyncButton
			>
		{/if}
	{/if}
	{#if error}
		<span class="text-error" role="alert">{error}</span>
	{/if}
</div>
