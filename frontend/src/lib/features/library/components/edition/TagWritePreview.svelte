<!--
	Writing the chosen edition's tags in a folder DroppedNeedle does not
	write by itself, as the folder's profile writes them: a preview of what
	changes in each file, then the write, then its undo. Nothing is written
	until "Write tags".
-->
<script lang="ts">
	import { ArrowRight, FileAudio, Undo2 } from 'lucide-svelte';
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import {
		getTagWriteQuery,
		createPreviewEditionTagsMutation,
		createUndoTagWriteMutation,
		createWriteEditionTagsMutation,
		type TagWriteJob
	} from '$lib/features/library/queries/EditionQueries.svelte';
	import {
		alreadyTaggedSentence,
		failureSentence,
		fileExtras,
		reasonSentence,
		tagChange,
		tagWriteBusySentence,
		tagsSentence,
		writeTagsLabel
	} from './editionCopy';

	interface Props {
		album: { userId: string | null | undefined; localId: string; rgMbid?: string };
		/** The album's edition as its status showed it. */
		edition: { releaseMbid: string; identityRevision: number };
		/** The files are still being placed on the edition: no preview yet. */
		placing: boolean;
		/** The id of the sentence that says why the preview waits. */
		placingSentenceId: string;
	}

	let { album, edition, placing, placingSentenceId }: Props = $props();

	const preview = createPreviewEditionTagsMutation();
	const write = createWriteEditionTagsMutation();
	const undo = createUndoTagWriteMutation();

	/** The job on screen: a preview of the write, or of its undo. */
	let current = $state<{ id: string; token: string } | null>(null);
	/** The write that can be undone. */
	let written = $state<{ id: string; rowRevision: number } | null>(null);
	let undone = $state(false);
	let error = $state<string | null>(null);
	let applyingUndo = $state<string | null>(null);

	const query = getTagWriteQuery(
		() => album.userId,
		() => album.localId,
		() => current?.id ?? null
	);
	const job = $derived<TagWriteJob | null>(
		current && query.data?.job_id === current.id ? query.data : null
	);
	const busy = $derived(
		current && (!job || job.step === 'waiting' || job.step === 'planning' || job.step === 'writing')
			? tagWriteBusySentence(job?.step ?? 'waiting', job?.undo ?? false)
			: null
	);
	const planned = $derived(job && !job.undo && job.step === 'ready' ? job : null);
	const blocked = $derived(planned?.files.filter((file) => file.blocked) ?? []);
	const changed = $derived(
		planned?.files.filter(
			(file) =>
				!file.blocked &&
				(file.changes.length ||
					file.added_files.length ||
					file.removed_tags.length ||
					file.artwork_changed)
		) ?? []
	);
	// A refusal of the whole album is said once, not on every file.
	const albumBlock = $derived.by(() => {
		const sentences = new Set(
			blocked.map((file) => (file.blocked ? reasonSentence(file.blocked) : ''))
		);
		const [only] = sentences;
		return blocked.length > 1 && sentences.size === 1 ? only : null;
	});
	const unchanged = $derived((planned?.files.length ?? 0) - changed.length - blocked.length);

	// Where the job ends: an undo that is planned applies at once; a write
	// waits for "Write tags"; a refusal or failure says why. A write that
	// failed part way can still be undone.
	$effect(() => {
		if (!job || !current) return;
		if (job.step === 'failed') {
			error = job.reason ? reasonSentence(job.reason) : "The tags couldn't be written.";
			if (job.undo_available) {
				written = { id: job.job_id, rowRevision: job.row_revision };
			}
			current = null;
		} else if (job.step === 'done' && job.undo) {
			written = null;
			undone = true;
			current = null;
		} else if (job.step === 'done') {
			written = { id: job.job_id, rowRevision: job.row_revision };
			current = null;
		} else if (job.step === 'ready' && !job.ready_to_apply) {
			error = job.stale_reasons[0] ?? 'This preview expired. Preview the new tags again.';
			current = null;
		} else if (job.step === 'ready' && job.undo && applyingUndo !== job.job_id) {
			const stop = job.files.find((file) => file.blocked)?.blocked;
			if (stop) {
				error = reasonSentence(stop);
				current = null;
			} else {
				applyingUndo = job.job_id;
				void applyJob(job, current.token, "The tags couldn't be put back.");
			}
		}
	});

	async function applyJob(target: TagWriteJob, token: string, fallback: string): Promise<void> {
		try {
			await write.mutateAsync({
				...album,
				jobId: target.job_id,
				previewToken: token,
				rowRevision: target.row_revision
			});
			await query.refetch();
		} catch (failure) {
			error = failureSentence(failure, fallback);
			current = null;
		}
	}

	async function showPreview(): Promise<void> {
		error = null;
		undone = false;
		try {
			const { userId, localId, rgMbid } = album;
			const queued = await preview.mutateAsync({ userId, localId, rgMbid, ...edition });
			current = { id: queued.job_id, token: queued.preview_token };
		} catch (failure) {
			error = failureSentence(failure, "The new tags couldn't be previewed.");
		}
	}

	async function writeTags(): Promise<void> {
		if (!planned || !current) return;
		error = null;
		await applyJob(planned, current.token, "The tags couldn't be written.");
	}

	async function undoWrite(): Promise<void> {
		if (!written) return;
		error = null;
		try {
			const queued = await undo.mutateAsync({
				...album,
				jobId: written.id,
				rowRevision: written.rowRevision
			});
			current = { id: queued.job_id, token: queued.preview_token };
		} catch (failure) {
			error = failureSentence(failure, "The tags couldn't be put back.");
		}
	}

	function fileName(path: string): string {
		return path.slice(path.lastIndexOf('/') + 1);
	}
</script>

<div class="flex w-full flex-col items-start gap-3">
	{#if busy}
		<span class="flex items-center gap-2 text-base-content/80">
			<span class="loading loading-spinner loading-xs" aria-hidden="true"></span>
			<span>{busy}</span>
		</span>
	{:else if written}
		<span class="flex flex-wrap items-center gap-2">
			<span>Tags written.</span>
			<AsyncButton variant="ghost" size="xs" icon={Undo2} onclick={undoWrite}>Undo</AsyncButton>
		</span>
	{:else}
		<span class="text-base-content/80"
			>{undone ? 'The old tags are back.' : tagsSentence('manual')}</span
		>
		{#if planned}
			{#if changed.length}
				<ul class="flex w-full flex-col gap-2" aria-label="Tags that change">
					{#each changed as file (file.path)}
						<li class="overflow-hidden rounded-box border border-base-content/10 bg-base-200/60">
							<span
								class="flex items-center gap-2 border-b border-base-content/10 px-3 py-2 font-medium"
							>
								<FileAudio class="h-4 w-4 shrink-0 text-muted" aria-hidden="true" />
								<span class="min-w-0 break-all">{fileName(file.path)}</span>
							</span>
							<dl class="grid grid-cols-[minmax(0,8rem)_minmax(0,1fr)] gap-x-3 gap-y-1.5 px-3 py-2">
								{#each file.changes as change (change.field)}
									{@const line = tagChange(change)}
									<dt class="text-muted">{line.label}</dt>
									<dd class="flex min-w-0 flex-wrap items-baseline gap-x-1.5 break-words">
										{#if line.from !== null}
											<del class="text-muted">{line.from}</del>
											<ArrowRight
												class="h-3.5 w-3.5 shrink-0 self-center text-muted"
												aria-hidden="true"
											/>
											<span class="sr-only">becomes</span>
										{/if}
										<ins class="font-medium no-underline">{line.to}</ins>
									</dd>
								{/each}
								{#each fileExtras(file) as extra (extra.label)}
									<dt class="text-muted">{extra.label}</dt>
									<dd class="min-w-0 break-words">{extra.value}</dd>
								{/each}
							</dl>
						</li>
					{/each}
				</ul>
			{/if}
			{#if albumBlock}
				<span class="text-warning">{albumBlock}</span>
			{:else}
				{#each blocked as file (file.path)}
					<span class="text-warning"
						>{fileName(file.path)}: {file.blocked ? reasonSentence(file.blocked) : ''}</span
					>
				{/each}
			{/if}
			{#if unchanged}
				<span class="text-muted">{alreadyTaggedSentence(unchanged)}</span>
			{/if}
			{#if changed.length && !blocked.length}
				<AsyncButton variant="primary" size="sm" onclick={writeTags}
					>{writeTagsLabel(changed.length)}</AsyncButton
				>
			{/if}
		{:else}
			<AsyncButton
				variant="outline"
				size="sm"
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
